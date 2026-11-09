//! Pure GitHub identity, qualification and once-only source policy.
use super::mentions::explicit_mention;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::time::UNIX_EPOCH;
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
/// Already fetched canonical item and complete paginated histories.
pub struct Facts {
    pub repo: Value,
    pub issue: Value,
    pub events: Vec<Value>,
    pub comments: Vec<Value>,
    pub reviews: Vec<Value>,
    pub review_comments: Vec<Value>,
}
type Fallible<T> = Result<T, String>;
fn fail<T>(kind: &str) -> Fallible<T> {
    Err(kind.into())
}
fn text(v: &Value) -> Fallible<&str> {
    v.as_str().ok_or_else(|| "malformed".into())
}
fn id(v: &Value) -> Fallible<u64> {
    v.as_u64()
        .filter(|&n| n > 0)
        .ok_or_else(|| "malformed".into())
}
fn list(v: &Value) -> Fallible<&Vec<Value>> {
    v.as_array().ok_or_else(|| "malformed".into())
}
fn object(v: &Value) -> Fallible<&Map<String, Value>> {
    v.as_object().ok_or_else(|| "malformed".into())
}
fn user(v: &Value) -> Fallible<Option<u64>> {
    if v.is_null() {
        Ok(None)
    } else {
        object(v)?;
        Ok(Some(id(&v["id"])?))
    }
}
fn human(v: &Value, field: &str) -> Fallible<Option<u64>> {
    let found = user(&v[field])?;
    Ok(found.filter(|_| v[field]["type"] == "User" && v["performed_via_github_app"].is_null()))
}
fn bot(v: &Value, n: u64) -> bool {
    v["id"].as_u64() == Some(n) && v["type"] == "Bot"
}
fn app(v: &Value, n: u64) -> bool {
    v["id"].as_u64() == Some(n)
}
fn echo(v: &Value, n: u64) -> bool {
    v["actor"]["id"].as_u64() == Some(n) && v["actor"]["type"] == "User"
}
/// UTC epoch microseconds; precision beyond six fractional digits is truncated.
pub fn stamp(value: &str) -> Fallible<u64> {
    let normalized = if let Some(prefix) = value.strip_suffix("+00:00") {
        format!("{prefix}Z")
    } else {
        value.into()
    };
    let parsed = crate::bus::time::parse_utc(&normalized).ok_or_else(|| "timestamp".to_string())?;
    let millis = u64::try_from(
        parsed
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "timestamp")?
            .as_millis(),
    )
    .map_err(|_| "timestamp")?;
    let fraction = normalized
        .get(20..normalized.len() - 1)
        .filter(|_| normalized.as_bytes().get(19) == Some(&b'.'))
        .unwrap_or("");
    let micro = if fraction.len() > 3 {
        format!("{:0<3}", &fraction[3..fraction.len().min(6)])
            .parse::<u64>()
            .map_err(|_| "timestamp")?
    } else {
        0
    };
    millis
        .checked_mul(1000)
        .and_then(|v| v.checked_add(micro))
        .ok_or_else(|| "timestamp".into())
}
fn moment(v: &Value) -> Fallible<u64> {
    stamp(text(v)?)
}
fn utc(at: u64, automatic: bool) -> String {
    let s = crate::bus::time::format_utc(UNIX_EPOCH + std::time::Duration::from_micros(at));
    let base = &s[..19];
    if automatic && at.is_multiple_of(1_000_000) {
        format!("{base}+00:00")
    } else {
        format!("{base}.{:06}+00:00", at % 1_000_000)
    }
}
fn canonical(v: &Value, out: &mut String) {
    match v {
        Value::String(s) => {
            out.push('"');
            for ch in s.chars() {
                match ch {
                    '"' => out.push_str("\\\""),
                    '\\' => out.push_str("\\\\"),
                    '\n' => out.push_str("\\n"),
                    '\r' => out.push_str("\\r"),
                    '\t' => out.push_str("\\t"),
                    '\u{8}' => out.push_str("\\b"),
                    '\u{c}' => out.push_str("\\f"),
                    c if !(' '..='\u{7e}').contains(&c) => {
                        let mut units = [0u16; 2];
                        for unit in c.encode_utf16(&mut units) {
                            out.push_str(&format!("\\u{unit:04x}"));
                        }
                    }
                    c => out.push(c),
                }
            }
            out.push('"');
        }
        Value::Array(a) => {
            out.push('[');
            for (i, v) in a.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(v, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            out.push('{');
            let sorted: BTreeMap<_, _> = m.iter().collect();
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(&Value::String(k.clone()), out);
                out.push(':');
                canonical(v, out);
            }
            out.push('}');
        }
        Value::Number(n) if n.is_f64() => out.push_str(&python_float(&n.to_string())),
        _ => out.push_str(&v.to_string()),
    }
}
// JSON serializers differ in when they switch to exponent notation. Preserve
// Python's -4..15 fixed-point range and its signed, two-digit exponent spelling.
fn python_float(raw: &str) -> String {
    let (sign, unsigned) = raw.strip_prefix('-').map_or(("", raw), |s| ("-", s));
    let (mantissa, power) = unsigned.split_once('e').map_or((unsigned, 0), |(m, e)| {
        (m, e.parse::<i32>().unwrap_or_default())
    });
    let point = mantissa.find('.').unwrap_or(mantissa.len()) as i32;
    let digits: String = mantissa.chars().filter(|&c| c != '.').collect();
    let significant = digits.trim_start_matches('0');
    if significant.is_empty() {
        return format!("{sign}0.0");
    }
    let exponent = power + point - (digits.len() - significant.len()) as i32 - 1;
    let digits = significant.trim_end_matches('0');
    if !(-4..16).contains(&exponent) {
        let tail = if digits.len() > 1 {
            format!(".{}", &digits[1..])
        } else {
            String::new()
        };
        return format!("{sign}{}{tail}e{exponent:+03}", &digits[..1]);
    }
    let point = exponent + 1;
    if point <= 0 {
        format!("{sign}0.{}{digits}", "0".repeat((-point) as usize))
    } else if point as usize >= digits.len() {
        format!(
            "{sign}{digits}{}.0",
            "0".repeat(point as usize - digits.len())
        )
    } else {
        format!(
            "{sign}{}.{}",
            &digits[..point as usize],
            &digits[point as usize..]
        )
    }
}
/// Python `json.dumps(..., sort_keys=True, separators=(",", ":"))` source keys.
pub fn digest(values: &[Value]) -> String {
    let mut encoded = String::new();
    canonical(&Value::Array(values.to_vec()), &mut encoded);
    format!("{:x}", Sha256::digest(encoded.as_bytes()))
}
fn ids(v: &Value) -> Fallible<BTreeSet<u64>> {
    list(v)?.iter().map(id).collect()
}
/// Validate disjoint App/reviewer roles and return their sorted public records.
pub fn trust_classes(config: &Value) -> Fallible<Value> {
    let empty = json!([]);
    let apps = config.get("app_initiators").unwrap_or(&empty);
    let reviewers = config.get("trusted_reviewer_ids").unwrap_or(&empty);
    let apps = list(apps).map_err(|_| "config_app_initiators")?;
    let reviewers_list = list(reviewers).map_err(|_| "config_trusted_reviewers")?;
    let reviewers = ids(reviewers).map_err(|_| "config_trusted_reviewers")?;
    let mut app_ids = BTreeSet::new();
    let mut bots = BTreeSet::new();
    let mut people = ids(&config["trusted_actor_ids"])?;
    people.insert(id(&config["agent"]["id"])?);
    let mut sorted = Vec::new();
    for a in apps {
        let valid = (|| -> Fallible<Value> {
            let m = object(a)?;
            if m.len() != 3
                || !m.contains_key("app_id")
                || !m.contains_key("bot_user_id")
                || !m.contains_key("repository_ids")
            {
                return fail("malformed");
            }
            id(&a["app_id"])?;
            id(&a["bot_user_id"])?;
            let repos = ids(&a["repository_ids"])?;
            if repos.is_empty() || repos.len() != list(&a["repository_ids"])?.len() {
                return fail("malformed");
            }
            Ok(json!({"app_id":a["app_id"],"bot_user_id":a["bot_user_id"],"repository_ids":repos}))
        })();
        let a = valid.map_err(|_| "config_app_initiators")?;
        if !app_ids.insert(id(&a["app_id"])?) || !bots.insert(id(&a["bot_user_id"])?) {
            return fail("config_class_overlap");
        }
        sorted.push(a);
    }
    if reviewers.len() != reviewers_list.len()
        || !people.is_disjoint(&bots)
        || !people.is_disjoint(&reviewers)
        || !bots.is_disjoint(&reviewers)
    {
        return fail("config_class_overlap");
    }
    sorted.sort_by_key(|a| a["app_id"].as_u64());
    Ok(json!({"app_initiators":sorted,"trusted_reviewer_ids":reviewers}))
}
/// Enrollment names and source-validation policies for a validated config.
pub fn members(config: &Value) -> Value {
    let mut out = Map::new();
    if let Some(apps) = config["app_initiators"].as_array() {
        for a in apps {
            if let Some(n) = a["app_id"].as_u64() {
                out.insert(
                    format!("app:{n}"),
                    json!({"app_id":n,"bot":a["bot_user_id"],"repos":a["repository_ids"]}),
                );
            }
        }
    }
    if let Some(reviewers) = config["trusted_reviewer_ids"].as_array() {
        for r in reviewers {
            if let Some(n) = r.as_u64() {
                out.insert(format!("reviewer:{n}"), json!({"reviewer":n}));
            }
        }
    }
    Value::Object(out)
}
fn full_name(s: &str) -> bool {
    let Some((owner, repo)) = s.split_once('/') else {
        return false;
    };
    !owner.is_empty()
        && owner.len() <= 39
        && owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && !repo.is_empty()
        && repo.len() <= 100
        && repo != "."
        && repo != ".."
        && repo
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn parent(raw: &Value, field: &str, expected: &str) -> Fallible<u64> {
    let n = id(&raw["id"])?;
    if !text(&raw[field])?.eq_ignore_ascii_case(expected) {
        return fail("identity");
    }
    Ok(n)
}
fn body(raw: &Value) -> Fallible<Value> {
    if raw["body"].is_null() {
        Ok(Value::Null)
    } else {
        text(&raw["body"])?;
        Ok(raw["body"].clone())
    }
}
struct Observer<'a> {
    repo_id: u64,
    number: u64,
    canonical: &'a str,
    observations: Vec<Value>,
}
impl Observer<'_> {
    #[allow(clippy::too_many_arguments)]
    fn add(
        &mut self,
        kind: &str,
        signal: Option<&str>,
        object_id: String,
        version: String,
        actor: u64,
        at: u64,
        proof: Value,
        initiator: String,
    ) {
        self.observations.push(json!({"key":digest(&[json!("github.com"),json!(self.repo_id),json!(kind),json!(object_id),json!(version)]),"kind":kind,"signal":signal,"object_id":object_id,"version":version,"actor_id":actor,"occurred_at":utc(at,false),"repo_id":self.repo_id,"number":self.number,"full_name":self.canonical,"proof":proof,"initiator":initiator}));
    }
}
#[derive(Clone)]
struct Review {
    id: u64,
    at: u64,
    raw: Value,
}
#[derive(Clone)]
struct Interval {
    opened: u64,
    closed: Option<u64>,
    review: Option<Review>,
    handoff: Value,
}
fn handoffs(
    facts: &[(&str, u64, u64, Option<u64>)],
    reviews: &BTreeMap<u64, Vec<Review>>,
    trusted: &BTreeSet<u64>,
) -> BTreeMap<u64, Vec<Interval>> {
    // Requests continue an open interval. A removal/closure at the request's
    // exact time closes it; reviews must be strictly later, with ID tie-breaking.
    let mut out = BTreeMap::new();
    for &who in trusted {
        let ends: Vec<_> = facts
            .iter()
            .filter(|(kind, _, _, person)| {
                *kind == "close" || (*kind == "removal" && *person == Some(who))
            })
            .map(|(_, at, _, _)| *at)
            .collect();
        let mut submitted = reviews.get(&who).cloned().unwrap_or_default();
        submitted.sort_by_key(|r| (r.at, r.id));
        let mut requests: Vec<_> = facts
            .iter()
            .filter(|(kind, _, _, person)| *kind == "request" && *person == Some(who))
            .map(|(_, at, n, _)| (*at, *n))
            .collect();
        requests.sort();
        let mut intervals: Vec<Interval> = Vec::new();
        for (at, event_id) in requests {
            if intervals
                .last()
                .is_some_and(|i| i.closed.is_none_or(|closed| at < closed))
            {
                continue;
            }
            let first = submitted.iter().find(|r| r.at > at);
            let closed = ends
                .iter()
                .copied()
                .filter(|&end| end >= at)
                .chain(first.map(|r| r.at))
                .min();
            let review = first
                .filter(|r| closed.is_none_or(|end| r.at <= end))
                .cloned();
            intervals.push(Interval{opened:at,closed,review,handoff:json!({"request_event_id":event_id,"requested_at":utc(at,false),"reviewer_id":who})});
        }
        out.insert(who, intervals);
    }
    out
}
/// Qualify supplied facts without I/O. Identity is checked before source policy.
pub fn verify_item(
    config: &Value,
    owner_policy: &Value,
    repo_id: u64,
    full: &str,
    number: u64,
    f: &Facts,
) -> Fallible<Value> {
    let agent = id(&config["agent"]["id"])?;
    let login = text(&config["agent"]["login"])?;
    let owner = &owner_policy["owner"];
    let owner_login = text(&owner["login"])?;
    let owner_id = id(&owner["id"])?;
    let mut trusted = ids(&config["trusted_actor_ids"])?;
    trusted.remove(&agent);
    let empty = json!([]);
    let mut reviewers = ids(config.get("trusted_reviewer_ids").unwrap_or(&empty))?;
    reviewers.retain(|r| *r != agent && !trusted.contains(r));
    let returning: BTreeSet<_> = trusted.union(&reviewers).copied().collect();
    if repo_id == 0
        || number == 0
        || !full_name(full)
        || !full
            .split('/')
            .next()
            .unwrap_or_default()
            .eq_ignore_ascii_case(owner_login)
    {
        return fail("malformed");
    }
    let repo = &f.repo;
    let canonical = text(&repo["full_name"])?;
    if repo["id"].as_u64() != Some(repo_id)
        || repo["owner"]["id"].as_u64() != Some(owner_id)
        || repo["owner"]["type"] != owner["type"]
        || !text(&repo["owner"]["login"])?.eq_ignore_ascii_case(owner_login)
        || !full_name(canonical)
        || !canonical
            .split('/')
            .next()
            .unwrap_or_default()
            .eq_ignore_ascii_case(owner_login)
    {
        return fail("identity");
    }
    let root = format!("https://api.github.com/repos/{canonical}").to_ascii_lowercase();
    let pull = format!("{root}/pulls/{number}");
    let thread = format!("{root}/issues/{number}");
    let issue = &f.issue;
    id(&issue["id"])?;
    if issue["number"].as_u64() != Some(number)
        || !text(&issue["repository_url"])?.eq_ignore_ascii_case(&root)
        || !text(&issue["url"])?.eq_ignore_ascii_case(&thread)
    {
        return fail("identity");
    }
    let state = text(&issue["state"])?;
    if !["open", "closed"].contains(&state) {
        return fail("malformed");
    }
    let assigned = list(&issue["assignees"])?
        .iter()
        .map(|a| user(a)?.ok_or_else(|| "malformed".to_string()))
        .collect::<Fallible<Vec<_>>>()?
        .contains(&agent);
    let is_pr = !issue["pull_request"].is_null();
    if is_pr && !text(&issue["pull_request"]["url"])?.eq_ignore_ascii_case(&pull) {
        return fail("identity");
    }
    let mut obs = Observer {
        repo_id,
        number,
        canonical,
        observations: Vec::new(),
    };
    let mut present = Vec::new();
    let role = |who: u64| {
        if reviewers.contains(&who) {
            format!("reviewer:{who}")
        } else {
            "human".into()
        }
    };
    let apps = list(config.get("app_initiators").unwrap_or(&empty))?;
    let mut selected = None;
    for a in apps {
        let bot_id = id(&a["bot_user_id"])?;
        let app_id = id(&a["app_id"])?;
        if !is_pr
            && ids(&a["repository_ids"])?.contains(&repo_id)
            && bot(&issue["user"], bot_id)
            && app(&issue["performed_via_github_app"], app_id)
        {
            selected = Some((bot_id, app_id));
            break;
        }
    }
    let mut candidates: BTreeMap<u64, (u64, &Value)> = BTreeMap::new();
    let mut bodies: BTreeMap<u64, &Value> = BTreeMap::new();
    let mut conflicted = false;
    let merge_after = config
        .get("merge_wakes_after")
        .filter(|v| !v.is_null())
        .map(moment)
        .transpose()?;
    let own_pr = merge_after.is_some() && is_pr && human(issue, "user")? == Some(agent);
    let mut history = Vec::new();
    let mut bounds = Vec::new();
    for raw in &f.events {
        let event_id = id(&raw["id"])?;
        parent(raw, "url", &format!("{root}/issues/events/{event_id}"))?;
        present.push(format!("event:{event_id}"));
        if selected.is_some()
            && let Some(prev) = bodies.insert(event_id, raw)
        {
            conflicted |= prev != raw;
        }
        let name = text(&raw["event"])?;
        let at = moment(&raw["created_at"])?;
        let mut actor = user(&raw["actor"])?;
        match name {
            "assigned" | "unassigned" => {
                if user(&raw["assignee"])? != Some(agent) {
                    continue;
                }
                if name == "unassigned" {
                    bounds.push(at);
                    continue;
                }
                if echo(raw, agent) {
                    actor = None;
                }
                if let Some((bot_id, app_id)) = selected {
                    let assigner = user(&raw["assigner"])?;
                    if assigner == Some(bot_id)
                        || actor == Some(bot_id)
                        || (assigner.is_none() && actor.is_none())
                        || app(&raw["performed_via_github_app"], app_id)
                    {
                        candidates.insert(event_id, (at, raw));
                        continue;
                    }
                }
                let assigner = human(raw, "assigner")?;
                if let Some(assigner) =
                    assigner.filter(|n| trusted.contains(n) && actor.is_none_or(|a| a == *n))
                {
                    obs.add(
                        "assignment",
                        Some("assignment"),
                        format!("event:{event_id}"),
                        digest(&[json!(event_id), json!(utc(at, true))]),
                        assigner,
                        at,
                        json!({"event_id":event_id}),
                        "human".into(),
                    );
                }
            }
            "review_requested" => {
                let requester = user(&raw["review_requester"])?;
                let reviewer = human(raw, "requested_reviewer")?;
                if requester == Some(agent)
                    && actor == Some(agent)
                    && reviewer.is_some_and(|r| returning.contains(&r))
                {
                    history.push(("request", at, event_id, reviewer));
                }
            }
            "review_request_removed" => {
                let reviewer = user(&raw["requested_reviewer"])?;
                if reviewer.is_some_and(|r| returning.contains(&r)) {
                    history.push(("removal", at, event_id, reviewer));
                }
            }
            "closed" | "merged" => {
                history.push(("close", at, event_id, None));
                bounds.push(at);
                if name == "merged"
                    && own_pr
                    && merge_after.is_some_and(|cut| at > cut)
                    && let Some(actor) = human(raw, "actor")?.filter(|n| trusted.contains(n))
                {
                    obs.add(
                        "merge",
                        Some("merge"),
                        format!("event:{event_id}"),
                        digest(&[json!(event_id), json!(utc(at, true))]),
                        actor,
                        at,
                        json!({"event_id":event_id}),
                        "human".into(),
                    );
                }
            }
            _ => {}
        }
    }
    // An ambiguous first App assignment blocks later attempts, and conflicting
    // representations of any event make firstness unprovable.
    if !conflicted
        && let (Some((bot_id, app_id)), Some((&event_id, &(at, raw)))) = (
            selected,
            candidates.iter().min_by_key(|(n, (at, _))| (*at, **n)),
        )
        && bot(&raw["assigner"], bot_id)
        && (bot(&raw["actor"], bot_id) || echo(raw, agent))
        && (raw.get("performed_via_github_app").is_none()
            || app(&raw["performed_via_github_app"], app_id))
    {
        obs.add(
            "assignment",
            Some("assignment"),
            format!("event:{event_id}"),
            digest(&[json!(event_id), json!(utc(at, true))]),
            bot_id,
            at,
            json!({"event_id":event_id,"app_id":app_id}),
            format!("app:{app_id}"),
        );
    }
    let mut reviews: BTreeMap<u64, Vec<Review>> = BTreeMap::new();
    if is_pr {
        for raw in &f.reviews {
            if raw["state"] == "PENDING" {
                continue;
            }
            let n = parent(raw, "pull_request_url", &pull)?;
            present.push(format!("review:{n}"));
            if let Some(author) = human(raw, "user")?.filter(|a| returning.contains(a))
                && !raw["submitted_at"].is_null()
            {
                reviews.entry(author).or_default().push(Review {
                    id: n,
                    at: moment(&raw["submitted_at"])?,
                    raw: raw.clone(),
                });
            }
        }
    }
    let intervals = handoffs(&history, &reviews, &returning);
    let matched: BTreeMap<u64, &Interval> = intervals
        .values()
        .flatten()
        .filter_map(|i| i.review.as_ref().map(|r| (r.id, i)))
        .collect();
    let after = config
        .get("issue_mentions_after")
        .filter(|v| !v.is_null())
        .map(moment)
        .transpose()?;
    if is_pr || after.is_some() {
        for raw in &f.comments {
            let n = parent(raw, "issue_url", &thread)?;
            present.push(format!("issue_comment:{n}"));
            let author = human(raw, "user")?;
            let body = body(raw)?;
            let Some(author) = author.filter(|n| returning.contains(n)) else {
                continue;
            };
            if !text(&raw["url"])?.eq_ignore_ascii_case(&format!("{root}/issues/comments/{n}")) {
                return fail("identity");
            }
            let created = moment(&raw["created_at"])?;
            let updated = moment(&raw["updated_at"])?;
            let interval = intervals.get(&author).and_then(|v| {
                v.iter()
                    .find(|i| i.opened < created && i.closed.is_none_or(|end| created < end))
            });
            // Handoff attribution wins over mentioning, with one source key.
            // An edited comment never becomes a fresh explicit mention.
            let (signal, mut proof) = if let Some(i) = interval {
                (Some("handoff"), i.handoff.clone())
            } else if reviewers.contains(&author) {
                continue;
            } else if body.as_str().is_some_and(|b| explicit_mention(b, login))
                && created == updated
                && (is_pr || after.is_some_and(|cut| created > cut))
            {
                (
                    Some("mention"),
                    json!({"comment_id":n,"agent_id":agent,"created_at":utc(created,false)}),
                )
            } else {
                (None, json!({}))
            };
            proof["object_type"] = json!("issue_comment");
            obs.add(
                "feedback",
                signal,
                format!("issue_comment:{n}"),
                digest(&[raw["updated_at"].clone(), body]),
                author,
                updated,
                proof,
                role(author),
            );
        }
    }
    if is_pr {
        for (&who, rs) in &reviews {
            for review in rs {
                let interval = matched.get(&review.id);
                if interval.is_none() && reviewers.contains(&who) {
                    continue;
                }
                let raw = &review.raw;
                let mut proof = json!({"object_type":"review","review_id":review.id});
                if let Some(i) = interval {
                    object(&i.handoff)?.iter().for_each(|(k, v)| {
                        proof[k] = v.clone();
                    });
                }
                obs.add(
                    "feedback",
                    interval.map(|_| "handoff"),
                    format!("review:{}", review.id),
                    digest(&[
                        body(raw)?,
                        raw["submitted_at"].clone(),
                        raw["commit_id"].clone(),
                    ]),
                    who,
                    review.at,
                    proof,
                    role(who),
                );
            }
        }
        for raw in &f.review_comments {
            let n = parent(raw, "pull_request_url", &pull)?;
            present.push(format!("review_comment:{n}"));
            let Some(author) = human(raw, "user")?.filter(|a| returning.contains(a)) else {
                continue;
            };
            let review_id = if raw["pull_request_review_id"].is_null() {
                None
            } else {
                Some(id(&raw["pull_request_review_id"])?)
            };
            let mut updated = moment(&raw["updated_at"])?;
            let interval = review_id
                .and_then(|n| matched.get(&n))
                .filter(|i| i.handoff["reviewer_id"].as_u64() == Some(author));
            if interval.is_none() && reviewers.contains(&author) {
                continue;
            }
            let mut proof = json!({"object_type":"review_comment","review_id":review_id});
            if let Some(i) = interval {
                for (k, v) in object(&i.handoff)? {
                    proof[k] = v.clone();
                }
                if let Some(review) = &i.review {
                    updated = updated.max(review.at);
                }
            }
            obs.add(
                "feedback",
                interval.map(|_| "handoff"),
                format!("review_comment:{n}"),
                digest(&[raw["updated_at"].clone(), body(raw)?]),
                author,
                updated,
                proof,
                role(author),
            );
        }
    }
    let signals: Vec<_> = obs
        .observations
        .iter()
        .filter(|s| {
            s["kind"] == "merge"
                || (!s["signal"].is_null()
                    && state == "open"
                    && (s["kind"] == "feedback"
                        || (assigned
                            && moment(&s["occurred_at"])
                                .is_ok_and(|at| bounds.iter().all(|&bound| bound < at)))))
        })
        .cloned()
        .collect();
    present.sort();
    Ok(
        json!({"repo":{"id":repo_id,"full_name":canonical,"owner_id":owner_id},"number":number,"is_pr":is_pr,"open":state=="open","assigned":assigned,"observations":obs.observations,"signals":signals,"present":present}),
    )
}
const SOURCE_FIELDS: &[&str] = &[
    "key",
    "kind",
    "object_id",
    "version",
    "actor_id",
    "occurred_at",
    "repo_id",
    "number",
    "full_name",
    "proof",
    "initiator",
];
fn source(
    v: &Value,
    snapshot: &Value,
    trusted: &Value,
    signal: bool,
    policy: &Value,
) -> Fallible<()> {
    let m = object(v).map_err(|_| "snapshot")?;
    if m.keys().filter(|k| k.as_str() != "signal").count() != SOURCE_FIELDS.len()
        || SOURCE_FIELDS.iter().any(|k| !m.contains_key(*k))
        || m.keys()
            .any(|k| k != "signal" && !SOURCE_FIELDS.contains(&k.as_str()))
    {
        return fail("snapshot");
    }
    if signal
        && !matches!(
            v["signal"].as_str(),
            Some("assignment" | "handoff" | "mention" | "merge")
        )
    {
        return fail("snapshot");
    }
    if !matches!(
        v["kind"].as_str(),
        Some("assignment" | "feedback" | "merge")
    ) || v["repo_id"] != snapshot["repo"]["id"]
        || v["number"] != snapshot["number"]
    {
        return fail("snapshot");
    }
    let proof = object(&v["proof"]).map_err(|_| "snapshot")?;
    if proof.values().any(|v| {
        !v.is_null()
            && !v.is_boolean()
            && !v.is_i64()
            && !v.is_u64()
            && !v.as_str().is_some_and(|s| s.chars().count() <= 256)
    }) {
        return fail("snapshot");
    }
    for k in ["key", "object_id", "version", "full_name"] {
        text(&v[k]).map_err(|_| "snapshot")?;
    }
    let initiator = text(&v["initiator"])?;
    let actor = id(&v["actor_id"])?;
    let member = policy.get(initiator);
    if initiator == "human" {
        if trusted.get(actor.to_string()).is_none()
            || (v["kind"] == "merge" && snapshot["is_pr"] != true)
        {
            return fail("untrusted_source");
        }
    } else {
        let Some(member) = member.filter(|_| trusted.get(initiator).is_some()) else {
            return fail("untrusted_source");
        };
        if member.get("bot").is_some() {
            if v["kind"] != "assignment"
                || (!v["signal"].is_null() && v["signal"] != "assignment")
                || v["actor_id"] != member["bot"]
                || !list(&member["repos"])?.contains(&snapshot["repo"]["id"])
                || snapshot["is_pr"] != false
                || v["proof"]["app_id"] != member["app_id"]
            {
                return fail("untrusted_source");
            }
        } else {
            if v["kind"] != "feedback"
                || snapshot["is_pr"] != true
                || v["signal"] != "handoff"
                || v["actor_id"] != member["reviewer"]
                || v["actor_id"] != v["proof"]["reviewer_id"]
            {
                return fail("untrusted_source");
            }
            moment(&v["proof"]["requested_at"])?;
        }
    }
    moment(&v["occurred_at"])?;
    Ok(())
}
fn fresh(row: &Value, repo: &Value, s: &Value, prior: Option<&Value>) -> Fallible<bool> {
    let initiator = text(&s["initiator"])?;
    let who = if initiator == "human" {
        id(&s["actor_id"])?.to_string()
    } else {
        initiator.into()
    };
    let enrolled = moment(&row["trusted_since"][&who])?;
    if initiator != "human" {
        let floor = if initiator.starts_with("reviewer:") {
            &s["proof"]["requested_at"]
        } else {
            &s["occurred_at"]
        };
        if moment(floor)? <= enrolled {
            return Ok(false);
        }
    }
    let cutoff = if repo["baseline"].is_null() {
        enrolled
    } else {
        enrolled.max(moment(&repo["baseline"])?)
    };
    let occurred = moment(&s["occurred_at"])?;
    if s["signal"] == "mention" {
        return Ok(occurred > cutoff.max(moment(&row["mention_since"])?)
            && prior.is_none_or(|p| p["kind"] != "feedback"));
    }
    let edited = if let Some(p) = prior {
        p["kind"] == s["kind"] && moment(&p["seen_at"])? >= cutoff
    } else {
        false
    };
    Ok(occurred > cutoff || edited)
}
/// Validate against a private copy, committing only a completely valid snapshot.
pub fn merge_snapshot(
    row: &mut Value,
    snapshot: &Value,
    now: &str,
    policy: &Value,
) -> Fallible<usize> {
    let mut next = row.clone();
    let queued = merge_inner(&mut next, snapshot, now, policy)?;
    *row = next;
    Ok(queued)
}
fn merge_inner(row: &mut Value, s: &Value, now: &str, policy: &Value) -> Fallible<usize> {
    stamp(now)?;
    object(policy)?;
    let repo_id = id(&s["repo"]["id"])?;
    id(&s["number"])?;
    text(&s["repo"]["full_name"])?;
    if !s["open"].is_boolean() || !s["is_pr"].is_boolean() {
        return fail("snapshot");
    }
    let repo = row["repos"]
        .get(repo_id.to_string())
        .filter(|r| r["status"] == "admitted")
        .ok_or_else(|| "repo_scope".to_string())?
        .clone();
    for k in ["trusted_since", "objects", "seen", "pending"] {
        object(&row[k])?;
    }
    let mut observed: BTreeMap<String, &Value> = BTreeMap::new();
    for v in list(&s["observations"])? {
        source(v, s, &row["trusted_since"], false, policy)?;
        let key = text(&v["key"])?;
        if observed.insert(key.into(), v).is_some_and(|prev| prev != v) {
            return fail("snapshot");
        }
    }
    if observed
        .values()
        .filter(|v| {
            v["initiator"]
                .as_str()
                .is_some_and(|s| s.starts_with("app:"))
        })
        .count()
        > 1
    {
        return fail("snapshot");
    }
    let mut eligible: BTreeMap<String, &Value> = BTreeMap::new();
    for v in list(&s["signals"])? {
        source(v, s, &row["trusted_since"], true, policy)?;
        let key = text(&v["key"])?;
        let Some(base) = observed.get(key) else {
            return fail("snapshot");
        };
        if SOURCE_FIELDS.iter().any(|k| base[*k] != v[*k])
            || eligible.insert(key.into(), v).is_some_and(|prev| prev != v)
        {
            return fail("snapshot");
        }
    }
    let present: BTreeSet<_> = list(&s["present"])?
        .iter()
        .map(text)
        .collect::<Fallible<_>>()?;
    let pending = object(&row["pending"])?;
    let mut cancel = Vec::new();
    let mut rename = Vec::new();
    for (key, p) in pending {
        object(p)?;
        id(&p["repo_id"])?;
        id(&p["number"])?;
        if p["repo_id"] != s["repo"]["id"] || p["number"] != s["number"] {
            continue;
        }
        let kind = text(&p["kind"])?;
        let source_key = text(&p["key"])?;
        let obj = text(&p["object_id"])?;
        if kind != "merge"
            && (s["open"] != true
                || (kind == "assignment" && !eligible.contains_key(source_key))
                || !present.contains(obj))
        {
            cancel.push(key.clone());
        } else {
            rename.push(key.clone());
        }
    }
    for k in cancel {
        row["pending"]
            .as_object_mut()
            .ok_or("malformed")?
            .remove(&k);
    }
    for k in rename {
        row["pending"][&k]["full_name"] = s["repo"]["full_name"].clone();
    }
    let previous = row["objects"].clone();
    let mut queued = 0;
    for (key, v) in observed {
        let composite = format!("{repo_id}:{key}");
        let target = format!("{repo_id}:{}", text(&v["object_id"])?);
        let signal = eligible.get(&key);
        let unseen = row["seen"].get(&composite).is_none();
        let fresh = if unseen {
            if let Some(signal) = signal {
                fresh(row, &repo, signal, previous.get(&target))?
            } else {
                false
            }
        } else {
            false
        };
        row["objects"][&target] = json!({"seen_at":now,"kind":v["kind"]});
        if !unseen {
            continue;
        }
        row["seen"][&composite] = json!(now);
        if fresh {
            row["pending"][&composite] = (*signal.ok_or("snapshot")?).clone();
            queued += 1;
        }
    }
    Ok(queued)
}
