//! Notification discovery and targeted revalidation. Feed and nominations commit together.
use super::{
    api::{self, Api, Fault, Result},
    model::{self, Config, State, now_ms, utc},
    qualify::{self, Facts},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    time::{Duration, UNIX_EPOCH},
};
fn id(v: &Value) -> Result<u64> {
    v.as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| "feed_shape".into())
}
fn text(v: &Value) -> Result<&str> {
    v.as_str().ok_or_else(|| "feed_shape".into())
}
pub fn verify_account(api: &mut Api<'_>) -> Result<()> {
    let a = api.get("/user")?;
    let expected = &api.cfg.settings.agent;
    if a["id"] != expected.id
        || !a["login"]
            .as_str()
            .unwrap_or("")
            .eq_ignore_ascii_case(&expected.login)
        || a["type"] != "User"
    {
        return Err("account_identity".into());
    }
    Ok(())
}
pub fn collect_feed(api: &mut Api<'_>, s: &mut State) -> Result<()> {
    let start = model::stamp(&s.feature_start)?;
    let since = crate::bus::time::format_utc(UNIX_EPOCH + Duration::from_secs(start / 1000000));
    let since = format!("{}Z", since.get(..19).ok_or("timestamp")?);
    let mut path =
        format!("/notifications?all=true&participating=false&per_page=50&page=1&since={since}");
    let floor = s
        .feed
        .last_complete
        .as_deref()
        .map(model::stamp)
        .transpose()?
        .map_or(start, |n| n.saturating_sub(600000000));
    let mut validator = None;
    let mut complete = None;
    for page in 0..api.cfg.settings.max_pages {
        let r = api.request(
            &path,
            if page == 0 {
                s.feed.validator.as_deref()
            } else {
                None
            },
        )?;
        s.feed.due_ms = now_ms().saturating_add(api.poll_seconds * 1000);
        if r.status == 304 {
            return Ok(());
        }
        if page == 0 {
            if r.body == json!([])
                && !r.headers.contains_key("last-modified")
                && !r.headers.contains_key("link")
            {
                s.feed.validator = None;
                return Ok(());
            }
            let v = r.headers.get("last-modified").ok_or("feed_metadata")?;
            let t = api::http_date(v).map_err(|_| Fault::from("feed_metadata"))?;
            validator = Some(v.clone());
            complete = Some(crate::bus::time::format_utc(
                UNIX_EPOCH + Duration::from_millis(t),
            ));
        }
        let threads = r.body.as_array().ok_or("feed_shape")?;
        let mut older = !threads.is_empty();
        for thread in threads {
            let updated = model::stamp(text(&thread["updated_at"])?)?;
            older &= updated < floor;
            let repo = &thread["repository"];
            if id(&repo["owner"]["id"])? != api.cfg.settings.owner_policy.owner.id
                || updated < floor
                || !matches!(
                    thread["subject"]["type"].as_str(),
                    Some("Issue" | "PullRequest")
                )
            {
                continue;
            }
            let name = text(&repo["full_name"])?;
            let repo_id = id(&repo["id"])?;
            let route = if thread["subject"]["type"] == "Issue" {
                "issues"
            } else {
                "pulls"
            };
            let url = text(&thread["subject"]["url"])?;
            let prefix = format!("https://api.github.com/repos/{name}/{route}/");
            let num = url
                .to_ascii_lowercase()
                .strip_prefix(&prefix.to_ascii_lowercase())
                .and_then(|s| s.parse::<u64>().ok())
                .filter(|n| *n > 0)
                .ok_or("feed_subject")?;
            if !model::full_name(name) {
                return Err("feed_subject".into());
            }
            let key = format!("{repo_id}:{num}");
            let tid = thread["id"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| thread["id"].as_u64().map(|n| n.to_string()))
                .ok_or("feed_shape")?;
            let version = format!("{tid}:{}", text(&thread["updated_at"])?);
            if s.row["hints"][&key] != version && s.row["dirty"].get(&key).is_none() {
                s.row["dirty"][&key] = json!({"repo_id":repo_id,"number":num,"full_name":name,"version":version,"at":utc(),"held":false});
            }
        }
        match api::next_page(
            r.headers.get("link").map_or("", String::as_str),
            &path,
            &BTreeMap::new(),
        )? {
            Some(p) if !older => path = p,
            _ => {
                s.feed.validator = validator;
                s.feed.last_complete = complete;
                return Ok(());
            }
        }
    }
    Err("feed_page_cap".into())
}
fn cancel(row: &mut Value, repo_id: u64) {
    row["pending"]
        .as_object_mut()
        .unwrap()
        .retain(|_, v| v["repo_id"] != repo_id);
}
pub fn excluded(cfg: &Config, row: &mut Value, repo: u64, name: &str) -> bool {
    for n in &cfg.settings.owner_policy.exclude_repositories {
        if n.eq_ignore_ascii_case(name) {
            let key = n.to_ascii_lowercase();
            if row["pins"].get(&key).is_none() {
                row["pins"][key] = json!(repo);
            }
        }
    }
    cfg.settings
        .owner_policy
        .exclude_repository_ids
        .contains(&repo)
        || row["pins"].as_object().unwrap().values().any(|v| v == repo)
}
pub fn targeted(api: &mut Api<'_>, row: &mut Value, pointer: &Value) -> Result<()> {
    let cfg = api.cfg;
    let repo_id = id(&pointer["repo_id"])?;
    let num = id(&pointer["number"])?;
    let name = text(&pointer["full_name"])?;
    let key = repo_id.to_string();
    let entry = &row["repos"][&key];
    if (entry.is_null() && cfg.settings.owner_policy.repository_policy != "include_new")
        || matches!(entry["status"].as_str(), Some("held" | "excluded"))
    {
        row["scope_skipped"] = json!(
            row["scope_skipped"]
                .as_u64()
                .ok_or("state_corrupt")?
                .saturating_add(1)
        );
        cancel(row, repo_id);
        return Ok(());
    }
    let repo = api.get(&format!("/repositories/{repo_id}"))?;
    let canonical = text(&repo["full_name"])?;
    let owner = &cfg.settings.owner_policy.owner;
    if repo["id"] != repo_id
        || repo["owner"]["id"] != owner.id
        || repo["owner"]["type"] != owner.kind
        || !text(&repo["owner"]["login"])?.eq_ignore_ascii_case(&owner.login)
        || !model::full_name(canonical)
        || !canonical
            .split('/')
            .next()
            .unwrap_or("")
            .eq_ignore_ascii_case(&owner.login)
    {
        return Err("identity".into());
    }
    let base = format!("/repos/{canonical}");
    api.aliases
        .insert(base.clone(), format!("/repositories/{repo_id}"));
    let issue = api.get(&format!("{base}/issues/{num}"))?;
    // Refuse a moved or foreign issue before spending quota on its histories.
    let root = format!("https://api.github.com{base}");
    if issue["number"] != num
        || !issue["repository_url"]
            .as_str()
            .unwrap_or("")
            .eq_ignore_ascii_case(&root)
        || !issue["url"]
            .as_str()
            .unwrap_or("")
            .eq_ignore_ascii_case(&format!("{root}/issues/{num}"))
        || (!issue["pull_request"].is_null()
            && !issue["pull_request"]["url"]
                .as_str()
                .unwrap_or("")
                .eq_ignore_ascii_case(&format!("{root}/pulls/{num}")))
    {
        return Err("identity".into());
    }
    let pr = !issue["pull_request"].is_null();
    let events = api.pages(&format!("{base}/issues/{num}/events"))?;
    let comments = if pr || cfg.settings.issue_mentions_after.is_some() {
        api.pages(&format!("{base}/issues/{num}/comments"))?
    } else {
        Vec::new()
    };
    let reviews = if pr {
        api.pages(&format!("{base}/pulls/{num}/reviews"))?
    } else {
        Vec::new()
    };
    let review_comments = if pr {
        api.pages(&format!("{base}/pulls/{num}/comments"))?
    } else {
        Vec::new()
    };
    let facts = Facts {
        repo,
        issue,
        events,
        comments,
        reviews,
        review_comments,
    };
    let config = cfg.value();
    let snap = qualify::verify_item(&config, &config["owner_policy"], repo_id, name, num, &facts)?;
    let canonical = text(&snap["repo"]["full_name"])?;
    if excluded(cfg, row, repo_id, name) || excluded(cfg, row, repo_id, canonical) {
        if !row["repos"][&key].is_null() {
            row["repos"][&key]["status"] = json!("excluded");
        }
        cancel(row, repo_id);
        return Ok(());
    }
    if row["repos"].get(&key).is_none() {
        row["repos"][&key] = json!({"full_name":canonical,"baseline":row["new_repo_cut"],"status":"admitted","listed":true});
    }
    row["repos"][&key]["full_name"] = json!(canonical);
    qualify::merge_snapshot(row, &snap, &utc(), &qualify::members(&config))?;
    Ok(())
}
pub fn hold_item(row: &mut Value, p: &Value, kind: &str) {
    let key = item_key(p);
    if row["dirty"].get(&key).is_none() {
        row["dirty"][&key] = json!({"repo_id":p["repo_id"],"number":p["number"],"full_name":p["full_name"],"version":format!("verify:{}",utc()),"at":utc(),"held":false});
    }
    row["dirty"][&key]["held"] = json!(true);
    model::hold(row, &format!("item:{key}"), kind);
}
pub fn item_key(v: &Value) -> String {
    format!("{}:{}", v["repo_id"], v["number"])
}
pub fn persist(cfg: &Config, s: &mut State) -> Result<()> {
    match model::save(cfg, s) {
        Ok(()) => Ok(()),
        Err(e) if e == "state_capacity" => {
            let mut previous = model::load(cfg)?;
            previous.fault = Some(e.clone());
            previous.feed.holds.insert(e.clone());
            model::save(cfg, &previous)?;
            *s = previous;
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}
pub fn collect(api: &mut Api<'_>, s: &mut State) -> Result<()> {
    if s.feed.holds.contains("state_capacity") {
        return Ok(());
    }
    if s.feed.holds.is_empty() && now_ms() >= s.feed.due_ms {
        let mut candidate = s.clone();
        match collect_feed(api, &mut candidate) {
            Ok(()) => *s = candidate,
            Err(e) => {
                s.feed.due_ms = (now_ms() + api.poll_seconds * 1000).max(e.retry_ms);
                s.fault = Some(e.kind.clone());
                if !api::stop(&e.kind) && !api::transient(&e.kind) {
                    s.feed.holds.insert(e.kind.clone());
                }
                persist(api.cfg, s)?;
                if api::stop(&e.kind) {
                    return Err(e);
                }
            }
        }
        persist(api.cfg, s)?;
    }
    let mut pointers: Vec<_> = s.row["dirty"]
        .as_object()
        .ok_or("state_corrupt")?
        .iter()
        .filter(|(_, v)| v["held"] == false)
        .map(|(k, v)| {
            (
                v["at"].as_str().unwrap_or("").to_owned(),
                k.clone(),
                v.clone(),
            )
        })
        .collect();
    pointers.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
    for (_, key, p) in pointers {
        match targeted(api, &mut s.row, &p) {
            Ok(()) => {
                s.row["hints"][&key] = p["version"].clone();
                s.row["dirty"].as_object_mut().unwrap().remove(&key);
                s.row["hold"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("item:{key}"));
            }
            Err(e) => {
                s.fault = Some(e.kind.clone());
                if api::stop(&e.kind) {
                    persist(api.cfg, s)?;
                    return Err(e);
                }
                if !api::transient(&e.kind) {
                    hold_item(&mut s.row, &p, &e.kind);
                }
            }
        }
        persist(api.cfg, s)?;
    }
    Ok(())
}
pub fn verified_pending(api: &mut Api<'_>, s: &mut State) -> Result<Vec<String>> {
    let mut groups = BTreeMap::<String, (u64, Value)>::new();
    for p in s.row["pending"]
        .as_object()
        .ok_or("state_corrupt")?
        .values()
    {
        let item = item_key(p);
        if s.row["dirty"][&item]["held"] == true {
            continue;
        }
        let at = model::stamp(text(&p["occurred_at"])?)?;
        let entry = groups.entry(item).or_insert((at, p.clone()));
        entry.0 = entry.0.min(at);
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|(_, v)| v.0);
    let mut keys = Vec::new();
    for (item, (_, p)) in groups {
        if !s.row["pending"]
            .as_object()
            .ok_or("state_corrupt")?
            .values()
            .any(|v| item_key(v) == item)
        {
            continue;
        }
        if keys.len() >= 100 {
            break;
        }
        match targeted(api, &mut s.row, &p) {
            Ok(()) => {
                s.row["hold"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&format!("item:{item}"));
                let mut found: Vec<_> = s.row["pending"]
                    .as_object()
                    .unwrap()
                    .iter()
                    .filter(|(_, v)| item_key(v) == item)
                    .map(|(k, v)| Ok((model::stamp(text(&v["occurred_at"])?)?, k.clone())))
                    .collect::<Result<_>>()?;
                found.sort();
                keys.extend(found.into_iter().take(100 - keys.len()).map(|(_, k)| k));
            }
            Err(e) => {
                s.fault = Some(e.kind.clone());
                if api::stop(&e.kind) {
                    break;
                }
                if !api::transient(&e.kind) {
                    hold_item(&mut s.row, &p, &e.kind);
                }
            }
        }
    }
    s.backoff_ms = s.backoff_ms.max(api.retry_ms);
    s.attempts.retain(|k, _| s.row["pending"].get(k).is_some());
    persist(api.cfg, s)?;
    Ok(keys)
}
