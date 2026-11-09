#!/usr/bin/env -S uv run --no-project --script --python 3.11
import os

from intake import watch

watch(os.environ.get("GITHUB_INTAKE_CONFIG", ""))
