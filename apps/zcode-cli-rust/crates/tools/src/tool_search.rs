use super::tools::{boolean, check_cancel, keys, resolve, string, uint};
use crate::contract::ToolOutput;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::Read,
    path::Path,
    time::{Duration, Instant, SystemTime},
};
use tokio_util::sync::CancellationToken;

pub async fn search(
    cwd: &Path,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let token = cancel.child_token();
    let _guard = token.clone().drop_guard();
    let cwd = cwd.to_owned();
    let name = name.to_owned();
    let args = args.clone();
    let job = tokio::task::spawn_blocking(move || search_sync(&cwd, &name, &args, &token));
    tokio::select! {
        _=cancel.cancelled()=>bail!("Search cancelled"),
        result=tokio::time::timeout(Duration::from_secs(30),job)=>result.context("Search timed out")??,
    }
}
fn search_sync(
    cwd: &Path,
    name: &str,
    args: &Value,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    let start = Instant::now();
    keys(
        args,
        if name == "Glob" {
            &["pattern", "path"]
        } else {
            &[
                "pattern",
                "path",
                "glob",
                "output_mode",
                "-A",
                "-B",
                "-C",
                "context",
                "-n",
                "-i",
                "-o",
                "type",
                "head_limit",
                "offset",
                "multiline",
            ]
        },
    )?;
    let root = resolve(
        cwd,
        args.get("path")
            .map(|_| string(args, "path"))
            .transpose()?
            .unwrap_or("."),
    )?;
    if !root.exists() {
        bail!("Search path does not exist");
    }
    if name == "Glob" && !root.is_dir() {
        bail!("Glob path must be a directory");
    }
    let pattern = string(args, "pattern")?;
    let glob = if name == "Glob" {
        Some(pattern)
    } else {
        args.get("glob").map(|_| string(args, "glob")).transpose()?
    };
    let matcher = glob
        .map(|p| {
            globset::GlobBuilder::new(p)
                .literal_separator(false)
                .build()
                .map(|g| g.compile_matcher())
        })
        .transpose()?;
    let mut walk = ignore::WalkBuilder::new(&root);
    walk.hidden(false).follow_links(false).require_git(false);
    if let Some(t) = args.get("type") {
        let mut types = ignore::types::TypesBuilder::new();
        types.add_defaults();
        types.select(t.as_str().context("type must be a string")?);
        walk.types(types.build()?);
    }
    let display = |p: &Path| {
        p.strip_prefix(cwd)
            .unwrap_or(p)
            .to_string_lossy()
            .replace('\\', "/")
    };
    let mut truncated = false;
    if name == "Glob" {
        let mut matched = vec![];
        for entry in walk.build() {
            check_cancel(cancel)?;
            let e = entry?;
            if !e.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            if !matcher
                .as_ref()
                .unwrap()
                .is_match(e.path().strip_prefix(&root).unwrap_or(e.path()))
            {
                continue;
            }
            let modified = e.metadata()?.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            matched.push((modified, display(e.path())));
            if matched.len() > 100 {
                matched.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
                matched.truncate(100);
                truncated = true;
            }
        }
        matched.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let paths: Vec<_> = matched.into_iter().map(|p| p.1).collect();
        let mut text = if paths.is_empty() {
            "No files found".to_owned()
        } else {
            paths.join("\n")
        };
        if truncated {
            text.push_str(
                "\n(Results are truncated. Consider using a more specific path or pattern.)",
            );
        }
        return Ok(ToolOutput::new(
            text,
            json!({"durationMs":start.elapsed().as_millis() as u64,"numFiles":paths.len(),"filenames":paths,"truncated":truncated}),
        ));
    }
    let mode = args
        .get("output_mode")
        .map(|_| string(args, "output_mode"))
        .transpose()?
        .unwrap_or("files_with_matches");
    if !["content", "files_with_matches", "count"].contains(&mode) {
        bail!("Invalid output_mode");
    }
    let multiline = boolean(args, "multiline", false)?;
    let only = boolean(args, "-o", false)?;
    let numbers = boolean(args, "-n", true)?;
    let re = regex::RegexBuilder::new(pattern)
        .case_insensitive(boolean(args, "-i", false)?)
        .multi_line(true)
        .dot_matches_new_line(multiline)
        .size_limit(2 * 1024 * 1024)
        .build()?;
    let offset = uint(args, "offset", 0)? as usize;
    let requested_limit = uint(args, "head_limit", 250)? as usize;
    let limit = if requested_limit == 0 {
        usize::MAX
    } else {
        requested_limit
    };
    let context = uint(args, "context", uint(args, "-C", 0)?)? as usize;
    let before = uint(args, "-B", context as u64)?.min(10_000) as usize;
    let after = uint(args, "-A", context as u64)?.min(10_000) as usize;
    let mut entries = vec![];
    let mut filenames = BTreeSet::new();
    let mut matched_count = 0;
    let mut seen = 0;
    let mut bytes = 0;
    let mut push = |text: String, path: String| -> bool {
        seen += 1;
        if seen <= offset {
            return true;
        }
        if entries.len() >= limit || bytes + text.len() > 20_000 {
            truncated = true;
            return false;
        }
        bytes += text.len();
        entries.push(text);
        filenames.insert(path);
        true
    };
    'files: for entry in walk.build() {
        check_cancel(cancel)?;
        let e = entry?;
        if !e.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        if let Some(m) = &matcher
            && !m.is_match(e.path().strip_prefix(&root).unwrap_or(e.path()))
        {
            continue;
        }
        let mut data = Vec::new();
        std::fs::File::open(e.path())?
            .take(16 * 1024 * 1024 + 1)
            .read_to_end(&mut data)?;
        check_cancel(cancel)?;
        if data.contains(&0) {
            continue;
        }
        if data.len() > 16 * 1024 * 1024 {
            bail!("grep_too_large: narrow the search; file exceeds 16 MiB");
        }
        let content = String::from_utf8_lossy(&data);
        let path = display(e.path());
        if mode != "content" {
            let count = if multiline {
                re.find_iter(&content).count()
            } else {
                content.lines().filter(|line| re.is_match(line)).count()
            };
            if count == 0 {
                continue;
            }
            matched_count += count;
            if !push(
                if mode == "count" {
                    format!("{path}:{count}")
                } else {
                    path.clone()
                },
                path,
            ) {
                break 'files;
            }
            continue;
        }
        let lines: Vec<_> = content.lines().collect();
        let mut hits = BTreeSet::new();
        let mut only_matches = vec![];
        if multiline {
            let starts: Vec<_> = std::iter::once(0)
                .chain(
                    content
                        .bytes()
                        .enumerate()
                        .filter(|(_, b)| *b == b'\n')
                        .map(|(i, _)| i + 1),
                )
                .collect();
            for m in re.find_iter(&content) {
                check_cancel(cancel)?;
                let first = starts
                    .partition_point(|offset| *offset <= m.start())
                    .saturating_sub(1);
                let last = first + m.as_str().bytes().filter(|b| *b == b'\n').count();
                hits.extend(first..=last);
                if only {
                    only_matches.push((first, m.as_str().to_owned()));
                }
            }
        } else {
            for (i, line) in lines.iter().enumerate() {
                for m in re.find_iter(line) {
                    hits.insert(i);
                    if only && !m.is_empty() {
                        only_matches.push((i, m.as_str().to_owned()));
                    }
                }
            }
        }
        if hits.is_empty() {
            continue;
        }
        matched_count += hits.len();
        if only {
            for (i, m) in only_matches {
                if !push(
                    if numbers {
                        format!("{path}:{}:{m}", i + 1)
                    } else {
                        format!("{path}:{m}")
                    },
                    path.clone(),
                ) {
                    break 'files;
                }
            }
        } else {
            let mut visible = BTreeSet::new();
            for &i in &hits {
                visible.extend(
                    i.saturating_sub(before)
                        ..=i.saturating_add(after).min(lines.len().saturating_sub(1)),
                );
            }
            for i in visible {
                let Some(line) = lines.get(i) else { continue };
                let separator = ":";
                if !push(
                    if numbers {
                        format!("{path}{separator}{}{separator}{line}", i + 1)
                    } else {
                        format!("{path}{separator}{line}")
                    },
                    path.clone(),
                ) {
                    break 'files;
                }
            }
        }
    }
    let count = filenames.len();
    let text = entries.join("\n");
    let mut output = json!({"mode":mode,"durationMs":start.elapsed().as_millis() as u64,"numFiles":count,"filenames":if mode=="files_with_matches"{entries.clone()}else{vec![]},"truncated":truncated,"appliedLimit":requested_limit,"appliedOffset":offset,"numMatches":matched_count});
    if mode != "files_with_matches" {
        output["content"] = text.clone().into();
        if mode == "content" {
            output["numLines"] = entries.len().into();
        }
    }
    let mut model = if text.is_empty() {
        "No matches found".to_owned()
    } else {
        text
    };
    if truncated {
        model.push_str("\n(Results truncated; use offset/head_limit or narrow the search.)");
    }
    Ok(ToolOutput::new(model, output))
}
