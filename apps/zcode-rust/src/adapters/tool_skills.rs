use super::{extension_config as config, extension_plugins as plugins};
use crate::{
    contract::ToolOutput,
    domain::skills::{Skill, SkillCatalog, frontmatter},
};
use anyhow::{Result, ensure};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

struct Root {
    path: PathBuf,
    scope: String,
    plugin_name: Option<String>,
    plugin_root: Option<PathBuf>,
}
pub(super) async fn discover(cwd: &Path, cancel: &CancellationToken) -> Result<SkillCatalog> {
    tokio::select! {biased; _=cancel.cancelled()=>anyhow::bail!("Cancelled"), result=discover_inner(cwd,cancel)=>result}
}
async fn discover_inner(cwd: &Path, cancel: &CancellationToken) -> Result<SkillCatalog> {
    let config = config::load(cwd).await?;
    let mut catalog = SkillCatalog {
        enabled: config["features"]["skill"] != false && config["skills"]["enabled"] != false,
        include_instructions: config["skills"]["includeInstructions"] != false,
        metadata_budget: config["skills"]["metadataBudget"]
            .as_u64()
            .unwrap_or(20_000) as usize,
        skills: vec![],
    };
    if !catalog.enabled {
        return Ok(catalog);
    }
    let mut roots = config::strings(&config["skills"]["roots"])
        .into_iter()
        .map(|p| Root {
            path: config::resolve(cwd, p),
            scope: "workspace".into(),
            plugin_name: None,
            plugin_root: None,
        })
        .collect::<Vec<_>>();
    let mut bases = vec![(config::home(), "user")];
    bases.extend(
        config::project_directories(cwd)
            .await
            .into_iter()
            .map(|p| (p, "workspace")),
    );
    for (base, scope) in bases {
        for sub in [".zcode/skills", ".agents/skills"] {
            roots.push(Root {
                path: base.join(sub),
                scope: scope.into(),
                plugin_name: None,
                plugin_root: None,
            });
        }
    }
    for plugin in plugins::enabled(cwd, &config, cancel).await? {
        let mut paths = vec!["skills"];
        paths.extend(config::strings(&plugin.manifest["skills"]));
        for path in paths {
            let path = config::resolve(&plugin.root, path);
            if path.starts_with(&plugin.root) {
                roots.push(Root {
                    path,
                    scope: "plugin".into(),
                    plugin_name: Some(plugin.name.clone()),
                    plugin_root: Some(plugin.root.clone()),
                });
            }
        }
    }
    let mut disabled = BTreeSet::new();
    for group in ["skill", "skills"] {
        if let Some(entries) = config[group].as_object() {
            for (path, value) in entries {
                if value["enable"] == false {
                    let path = config::resolve(cwd, path);
                    if let Ok(real) = tokio::fs::canonicalize(&path).await {
                        disabled.insert(real);
                    }
                    disabled.insert(path);
                }
            }
        }
    }
    let mut seen = BTreeSet::new();
    for root in roots {
        super::tools::check_cancel(cancel)?;
        let mut paths = vec![root.path.join("SKILL.md")];
        if let Ok(mut entries) = tokio::fs::read_dir(&root.path).await {
            let mut children = vec![];
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name().to_string_lossy().into_owned();
                if (name.starts_with('.') && name != ".system")
                    || [
                        "node_modules",
                        "dist",
                        "build",
                        "out",
                        "target",
                        "vendor",
                        "coverage",
                        "__pycache__",
                    ]
                    .contains(&name.as_str())
                {
                    continue;
                }
                children.push(entry.path().join("SKILL.md"));
                ensure!(children.len() <= 10_000, "Skill root exceeds entry limit");
            }
            children.sort();
            paths.extend(children);
        }
        for path in paths {
            super::tools::check_cancel(cancel)?;
            if !seen.insert(path.clone()) || disabled.contains(&path) {
                continue;
            }
            if let Some(boundary) = &root.plugin_root
                && !plugins::contained_file(boundary, &path).await
            {
                continue;
            }
            if tokio::fs::canonicalize(&path)
                .await
                .is_ok_and(|p| disabled.contains(&p))
            {
                continue;
            }
            let Ok((content, _)) = read(&path, 100_000).await else {
                continue;
            };
            let (has_frontmatter, values, _) = frontmatter(&content);
            let name = values.get("name").cloned().or_else(|| {
                (!has_frontmatter)
                    .then(|| path.parent()?.file_name()?.to_str().map(str::to_owned))
                    .flatten()
            });
            let Some(name) = name else {
                continue;
            };
            let description = values.get("description").cloned().unwrap_or_default();
            if (has_frontmatter && description.is_empty())
                || description.encode_utf16().count() > 1024
            {
                continue;
            }
            catalog.skills.push(Skill {
                name,
                description,
                path: path.to_string_lossy().into_owned(),
                scope: root.scope.clone(),
                plugin_name: root.plugin_name.clone(),
                when_to_use: values.get("when_to_use").cloned(),
                plugin_root: root
                    .plugin_root
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            });
            ensure!(
                catalog.skills.len() <= 10_000,
                "Skill catalog exceeds entry limit"
            );
        }
    }
    // stable sort retains root precedence among identical names.
    catalog.skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(catalog)
}
pub(super) async fn load(
    skill: &Skill,
    name: &str,
    cancel: &CancellationToken,
) -> Result<ToolOutput> {
    tokio::select! {biased; _=cancel.cancelled()=>anyhow::bail!("Cancelled"), result=async {
        let path=Path::new(&skill.path);
        if let Some(root)=&skill.plugin_root {ensure!(plugins::contained_file(Path::new(root),path).await,"Plugin Skill path escaped its root");}
        let (content,truncated)=read(path,100_000).await?;
        let (_,_,content)=frontmatter(&content);
        let directory=path.parent().ok_or_else(||anyhow::anyhow!("Skill directory missing"))?.to_string_lossy();
        let content=content.replace("${ZCODE_SKILL_DIR}",&directory).replace("${CLAUDE_SKILL_DIR}",&directory);
        let parts=[format!("<skill_content name=\"{name}\">"),format!("# Skill: {name}"),content,format!("Base directory for this skill: {directory}"),"Relative paths in this skill are relative to this base directory.".into(),if truncated{"[Skill content truncated]".into()}else{String::new()},"</skill_content>".into()];
        Ok(ToolOutput::text(parts.into_iter().filter(|p|!p.is_empty()).collect::<Vec<_>>().join("\n")))
    }=>result}
}
async fn read(path: &Path, limit: u64) -> Result<(String, bool)> {
    let file = tokio::fs::File::open(path).await?;
    let metadata = file.metadata().await?;
    ensure!(metadata.is_file(), "Skill must be a regular file");
    let mut bytes = Vec::with_capacity(metadata.len().min(limit) as usize);
    file.take(limit + 1).read_to_end(&mut bytes).await?;
    let truncated = bytes.len() > limit as usize;
    bytes.truncate(limit as usize);
    Ok((String::from_utf8_lossy(&bytes).into_owned(), truncated))
}
