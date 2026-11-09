# /// script
# requires-python = ">=3.11"
# ///
"""Adapt usage-route's quota decision to the team's go/wait protocol."""

import json
import os
import re
import subprocess
import sys
from datetime import UTC, datetime
from pathlib import Path
from typing import Any


def model_class(vendor: str, model: str) -> str:
    if vendor == "codex":
        match = re.fullmatch(r"(?:gpt-.+-)?(astra|sol|terra|luna)", model)
        if match:
            return match[1]
    return model


def matches(candidate: Any, vendor: str, model: str) -> bool:
    return (
        isinstance(candidate, dict)
        and candidate.get("kind") == vendor
        and isinstance(candidate.get("model"), str)
        and model_class(vendor, candidate["model"]) == model_class(vendor, model)
    )


def timestamp(value: Any) -> datetime:
    if not isinstance(value, str):
        raise ValueError("missing observation time")
    try:
        parsed = datetime.fromisoformat(value)
    except ValueError:
        raise ValueError("invalid observation time") from None
    if parsed.tzinfo is None:
        raise ValueError("observation time has no timezone")
    return parsed


def decision(data: Any, vendor: str, model: str, now: datetime) -> str:
    if not isinstance(data, dict):
        raise ValueError("invalid reader response")
    if data.get("decision") != "route":
        raise ValueError("reader did not authorize this route")
    if not matches(data.get("route"), vendor, model):
        raise ValueError("reader returned a different vendor or model")
    observations = data.get("observations")
    observation = observations.get(vendor) if isinstance(observations, dict) else None
    if not isinstance(observation, dict) or observation.get("status") != "ok":
        raise ValueError("usage is unknown")
    if (
        not timestamp(observation.get("observed_at"))
        <= now
        < timestamp(observation.get("expires_at"))
    ):
        raise ValueError("usage observation is stale or in the future")
    evaluations = data.get("evaluations")
    if not isinstance(evaluations, list) or len(evaluations) != 1:
        raise ValueError("reader must evaluate exactly the requested route")
    evaluation = evaluations[0]
    if (
        not isinstance(evaluation, dict)
        or not matches(evaluation.get("candidate"), vendor, model)
        or evaluation.get("state") != "available"
    ):
        raise ValueError("requested quota scope is not available")
    constraints = evaluation.get("constraints")
    if not isinstance(constraints, list):
        raise ValueError("missing quota constraints")
    for constraint in constraints:
        if not isinstance(constraint, dict):
            raise ValueError("invalid quota constraint")
        used = constraint.get("used_percent")
        if (
            isinstance(used, bool)
            or not isinstance(used, (int, float))
            or not 0 <= used < 100
            or constraint.get("blocked") is not False
        ):
            raise ValueError("quota constraint is blocked or unknown")
    warnings = data.get("warnings")
    if not isinstance(warnings, list):
        raise ValueError("missing quota warnings")
    if evaluation.get("warnings") != warnings:
        raise ValueError("quota warnings disagree with evaluation")
    if constraints and not warnings:
        raise ValueError("constrained quota is missing its advisory")
    messages = []
    for warning in warnings:
        if not isinstance(warning, dict) or not isinstance(warning.get("message"), str):
            raise ValueError("invalid quota warning")
        messages.append(" ".join(warning["message"].split()))
    return "; ".join(["requested quota available", *messages])


def refusal(stdout: str, vendor: str, model: str, now: datetime) -> str:
    """Keep useful retry information without relaying arbitrary reader errors."""
    try:
        data = json.loads(stdout)
        if not isinstance(data, dict):
            return "reader unavailable or quota denied"
        if data.get("reason") in (
            "selected model/effort could not be validated by model-route",
            "model-route returned an invalid route",
        ):
            return "reader model validation failed"
        status = data.get("decision")
        reasons = {
            "blocked": "quota blocked",
            "unknown": "usage unknown",
            "suggest_wait": "reader recommends waiting",
            "invalid_policy": "reader configuration/model error",
        }
        reason = reasons.get(status) if isinstance(status, str) else None
        if reason is None:
            return "reader unavailable or quota denied"
        recovery = data.get("expected_recovery_at")
        if recovery is None and isinstance(data.get("evaluations"), list):
            for evaluation in data["evaluations"]:
                if isinstance(evaluation, dict) and matches(
                    evaluation.get("candidate"), vendor, model
                ):
                    recovery = evaluation.get("expected_recovery_at")
                    break
        if recovery is not None:
            reset = timestamp(recovery)
            if reset > now:
                return f"{reason}; check again at {reset.astimezone(UTC).isoformat()}"
        return reason
    except (ValueError, TypeError):
        return "reader unavailable or quota denied"


def reader_command() -> str:
    base = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config")
    setting = base / "borgchart" / "seat-reader"
    try:
        reader = setting.read_text().strip()
    except FileNotFoundError:
        if setting.is_symlink():
            raise ValueError("seat-reader setting is a broken symlink") from None
        return "usage-route"
    if not reader or "\n" in reader or "\r" in reader or "\0" in reader:
        raise ValueError("seat-reader must name one executable")
    if "/" in reader and not Path(reader).is_absolute():
        raise ValueError("seat-reader path must be absolute")
    return reader


def main(args: list[str]) -> int:
    if (
        len(args) != 3
        or args[0] != "route"
        or args[1] not in {"claude", "codex"}
        or not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9.-]*", args[2])
    ):
        print("wait: expected route <claude|codex> <model>")
        return 2
    _, vendor, model = args
    try:
        result = subprocess.run(
            [reader_command(), vendor, model],
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        if result.returncode != 0:
            raise ValueError(refusal(result.stdout, vendor, model, datetime.now(UTC)))
        reason = decision(json.loads(result.stdout), vendor, model, datetime.now(UTC))
    except (OSError, subprocess.SubprocessError, ValueError) as error:
        # Do not relay raw reader output, paths or errors that may contain secrets.
        reason = (
            str(error)
            if type(error) is ValueError
            else "reader unavailable or invalid response"
        )
        print(f"wait: {reason}")
        return 3
    print(f"go: {reason}")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
