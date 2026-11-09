//! GitHub CLI transport. Only sanitized error kinds leave this module.
use super::model::{Config, now_ms, stamp};
use crate::bounded_process::{self, Limits};
use serde_json::Value;
use std::{collections::BTreeMap, process::Command, time::Instant};
#[derive(Debug, Clone)]
pub struct Fault {
    pub kind: String,
    pub retry_ms: u64,
}
impl From<&str> for Fault {
    fn from(s: &str) -> Self {
        Self {
            kind: s.into(),
            retry_ms: 0,
        }
    }
}
impl From<String> for Fault {
    fn from(s: String) -> Self {
        Self {
            kind: s,
            retry_ms: 0,
        }
    }
}
pub type Result<T> = std::result::Result<T, Fault>;
pub fn transient(kind: &str) -> bool {
    kind.starts_with("http_5")
}
pub fn stop(kind: &str) -> bool {
    matches!(
        kind,
        "deadline" | "rate_limit" | "transport" | "http_401" | "account_identity" | "clock" | "sso"
    )
}
pub type Headers = BTreeMap<String, String>;
pub struct Response {
    pub empty: bool,
    pub status: u16,
    pub headers: Headers,
    pub body: Value,
}
pub fn http_date(value: &str) -> Result<u64> {
    let p: Vec<_> = value.split_whitespace().collect();
    if p.len() != 6 || p[5] != "GMT" {
        return Err("transport".into());
    }
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|m| *m == p[2])
    .ok_or("transport")?
        + 1;
    stamp(&format!("{}-{month:02}-{:0>2}T{}Z", p[3], p[1], p[4]))
        .map(|n| n / 1000)
        .map_err(Fault::from)
}
pub fn parse(raw: &[u8]) -> Result<Response> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| Fault::from("transport"))?
        .replace("\r\n", "\n");
    let (head, body) = text.split_once("\n\n").ok_or("transport")?;
    let mut lines = head.lines();
    let first = lines.next().ok_or("transport")?;
    let caps = regex::Regex::new(r"^HTTP/[0-9.]+ ([0-9]{3})(?: .*)?$")
        .unwrap()
        .captures(first)
        .ok_or("transport")?;
    let status = caps[1].parse().map_err(|_| Fault::from("transport"))?;
    let mut headers = Headers::new();
    for line in lines {
        let (name, value) = line.split_once(':').ok_or("transport")?;
        let name = name.trim().to_ascii_lowercase();
        let critical = matches!(
            name.as_str(),
            "date" | "link" | "last-modified" | "x-poll-interval" | "retry-after" | "x-github-sso"
        ) || name.starts_with("x-ratelimit-");
        if name.is_empty() || (headers.insert(name, value.trim().into()).is_some() && critical) {
            return Err("transport".into());
        }
    }
    let empty = body.trim().is_empty();
    let body = if empty {
        Value::Null
    } else {
        serde_json::from_str(body).map_err(|_| Fault::from("json"))?
    };
    Ok(Response {
        empty,
        status,
        headers,
        body,
    })
}
fn query(path: &str) -> Result<(&str, BTreeMap<String, Vec<String>>)> {
    let (path, q) = path.split_once('?').unwrap_or((path, ""));
    let mut result = BTreeMap::<String, Vec<String>>::new();
    for piece in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = piece.split_once('=').ok_or("pagination")?;
        result.entry(decode(k)?).or_default().push(decode(v)?);
    }
    Ok((path, result))
}
fn decode(s: &str) -> Result<String> {
    let mut bytes = Vec::new();
    let mut i = 0;
    while i < s.len() {
        match s.as_bytes()[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3).ok_or("pagination")?;
                bytes.push(u8::from_str_radix(hex, 16).map_err(|_| Fault::from("pagination"))?);
                i += 3
            }
            b'+' => {
                bytes.push(b' ');
                i += 1
            }
            c => {
                bytes.push(c);
                i += 1
            }
        }
    }
    String::from_utf8(bytes).map_err(|_| "pagination".into())
}
pub fn next_page(
    link: &str,
    path: &str,
    aliases: &BTreeMap<String, String>,
) -> Result<Option<String>> {
    if link.is_empty() {
        return Ok(None);
    }
    let re = regex::Regex::new(r#"<([^>]+)>;\s*rel="([a-z]+)""#).unwrap();
    if !re.replace_all(link, "").trim_matches([' ', ',']).is_empty() || !re.is_match(link) {
        return Err("pagination".into());
    }
    let (original, mut expected) = query(path)?;
    let current = expected.remove("page").unwrap_or(vec!["1".into()]);
    if current.len() != 1 {
        return Err("pagination".into());
    }
    let current = current[0]
        .parse::<u64>()
        .map_err(|_| Fault::from("pagination"))?;
    let mut next = None;
    for c in re.captures_iter(link) {
        let url = c[1]
            .strip_prefix("https://api.github.com")
            .ok_or("pagination")?;
        if !url.starts_with('/') || url.starts_with("//") || url.contains('#') {
            return Err("pagination".into());
        }
        let (found, mut q) = query(url)?;
        let page = q.remove("page").unwrap_or(vec!["1".into()]);
        let alias = aliases.iter().any(|(a, b)| {
            original
                .strip_prefix(&format!("{a}/"))
                .is_some_and(|tail| found == format!("{b}/{tail}"))
        });
        if (found != original && !alias)
            || q != expected
            || page.len() != 1
            || !page[0].bytes().all(|b| b.is_ascii_digit())
        {
            return Err("pagination".into());
        }
        let p = page[0]
            .parse::<u64>()
            .map_err(|_| Fault::from("pagination"))?;
        if &c[2] == "next" {
            if next.is_some() || Some(p) != current.checked_add(1) {
                return Err("pagination".into());
            }
            next = Some(url.to_owned());
        }
    }
    Ok(next)
}
pub struct Api<'a> {
    pub cfg: &'a Config,
    pub deadline: Instant,
    pub retry_ms: u64,
    pub poll_seconds: u64,
    pub aliases: BTreeMap<String, String>,
}
impl<'a> Api<'a> {
    pub fn new(cfg: &'a Config, deadline: Instant) -> Self {
        Self {
            cfg,
            deadline,
            retry_ms: 0,
            poll_seconds: cfg.settings.poll_seconds,
            aliases: BTreeMap::new(),
        }
    }
    pub fn raw(
        &self,
        path: &str,
        validator: Option<&str>,
        post: bool,
        deadline: Instant,
    ) -> Result<(Response, Option<i32>)> {
        if !regex::Regex::new(r"^/[A-Za-z0-9_./?=&%:+,-]+$")
            .unwrap()
            .is_match(path)
            || path.starts_with("//")
            || path.contains("..")
        {
            return Err("path".into());
        }
        if Instant::now() >= deadline {
            return Err("deadline".into());
        }
        let mut cmd = Command::new(&self.cfg.settings.gh[0]);
        cmd.args(&self.cfg.settings.gh[1..]).args([
            "api",
            "--hostname",
            "github.com",
            "--method",
            if post { "POST" } else { "GET" },
            "--include",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]);
        if let Some(v) = validator {
            if v.contains(['\r', '\n']) {
                return Err("validator".into());
            }
            cmd.args(["-H", &format!("If-Modified-Since: {v}")]);
        }
        if post {
            cmd.args(["-f", "content=eyes"]);
        }
        cmd.arg(&path[1..]);
        let output = bounded_process::run(
            &mut cmd,
            None,
            Limits {
                deadline,
                stdout_bytes: 8 * 1024 * 1024,
                stderr_bytes: 65536,
            },
        )
        .map_err(|e| {
            Fault::from(match e {
                bounded_process::Error::Deadline => "deadline",
                bounded_process::Error::OutputLimit(_) => "response_cap",
                bounded_process::Error::Io(_) => "transport",
            })
        })?;
        Ok((parse(&output.stdout)?, output.status.code()))
    }
    pub fn request(&mut self, path: &str, validator: Option<&str>) -> Result<Response> {
        if now_ms() < self.retry_ms {
            return Err(Fault {
                kind: "rate_limit".into(),
                retry_ms: self.retry_ms,
            });
        }
        let (r, accepted) = self.raw(path, validator, false, self.deadline)?;
        let date = http_date(r.headers.get("date").ok_or("transport")?)?;
        if now_ms().abs_diff(date) > 300000 {
            return Err("clock".into());
        }
        if let Some(n) = r
            .headers
            .get("x-poll-interval")
            .and_then(|s| s.parse::<u64>().ok())
        {
            self.poll_seconds = self.poll_seconds.max(n.min(86400));
        }
        if r.headers
            .get("x-ratelimit-remaining")
            .is_some_and(|s| s == "0")
        {
            self.retry_ms = self.retry_ms.max(now_ms() + 60000).max(
                r.headers
                    .get("x-ratelimit-reset")
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0)
                    .saturating_mul(1000),
            );
        }
        let limited = matches!(r.status, 403 | 429)
            && (r.headers.contains_key("retry-after")
                || self.retry_ms > now_ms()
                || r.status == 429
                || r.body["message"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains("secondary rate limit"));
        if limited {
            self.retry_ms = self.retry_ms.max(
                now_ms().saturating_add(
                    r.headers
                        .get("retry-after")
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(60)
                        .max(60)
                        .saturating_mul(1000),
                ),
            );
            return Err(Fault {
                kind: "rate_limit".into(),
                retry_ms: self.retry_ms,
            });
        }
        if r.headers.contains_key("x-github-sso") {
            return Err("sso".into());
        }
        if r.status == 304 && validator.is_some() && matches!(accepted, Some(0 | 1)) && r.empty {
            return Ok(r);
        }
        if r.status != 200 || accepted != Some(0) {
            return Err(format!("http_{}", r.status).into());
        }
        Ok(r)
    }
    pub fn get(&mut self, path: &str) -> Result<Value> {
        let r = self.request(path, None)?;
        if !r.body.is_object() {
            return Err("shape".into());
        }
        Ok(r.body)
    }
    pub fn pages(&mut self, path: &str) -> Result<Vec<Value>> {
        let mut path = format!(
            "{path}{}per_page=100&page=1",
            if path.contains('?') { '&' } else { '?' }
        );
        let mut result = Vec::new();
        for _ in 0..self.cfg.settings.max_pages {
            let r = self.request(&path, None)?;
            let a = r.body.as_array().ok_or("shape")?;
            if a.iter().any(|v| !v.is_object()) {
                return Err("shape".into());
            }
            result.extend(a.iter().cloned());
            match next_page(
                r.headers.get("link").map_or("", String::as_str),
                &path,
                &self.aliases,
            )? {
                Some(p) => path = p,
                None => return Ok(result),
            }
        }
        Err("page_cap".into())
    }
}
