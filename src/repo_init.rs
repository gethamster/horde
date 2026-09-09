//! Install repository-local agent instructions, bundled skills, and an MCP connection.

mod bundled;
mod config;

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const BEGIN: &str = "<!-- BEGIN HORDE DELEGATION -->";
const END: &str = "<!-- END HORDE DELEGATION -->";
const POLICY: &str = "## Delegate repository changes to Horde\n\n\
First determine whether you are already a Horde worker. If HORDE_WORKER_TOKEN\n\
is set or the runtime gave you a worker identity and an assigned Horde step,\n\
follow the horde-worker skill and perform that step. Do not\n\
resubmit the assignment as a new root task.\n\n\
For the agent talking to the user: for every user-requested repository change,\n\
use the horde skill and delegate implementation, tests, and review to Horde. This\n\
includes small edits, documentation, bug fixes, features, and refactoring, and\n\
overrides the skill's default advice to handle small edits locally. Do not\n\
implement changes directly or substitute another subagent system unless the\n\
user explicitly requests it. Read-only investigation and answering questions\n\
may happen in the current session.\n\n\
The agent talking to the user owns clarification, task submission, monitoring,\n\
answering worker questions within the user's instructions, and inspecting\n\
results. Include the user's constraints and acceptance checks in the submitted\n\
task. Follow it until completion or a blocker requiring user input. Verify the\n\
integrated result and report its branch, checks, and unresolved issues. Keep\n\
external delivery within the user's authorization.\n\n\
If Horde is unavailable, report the blocker instead of silently doing the work\n\
directly.\n";

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn paths(self) -> (&'static str, &'static str, &'static str, &'static str) {
        match self {
            Self::Codex => (
                "AGENTS.md",
                ".agents/skills",
                ".codex/config.toml",
                ".codex/.horde-init.json",
            ),
            Self::Claude => (
                "CLAUDE.md",
                ".claude/skills",
                ".mcp.json",
                ".claude/.horde-init.json",
            ),
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u32,
    files: BTreeMap<String, String>,
}

struct PlannedWrite {
    relative: PathBuf,
    bytes: Vec<u8>,
}

/// Preflight every destination and content conflict before changing any files.
pub fn install(repo: &Path, agent: Agent, data_dir: Option<&Path>) -> Result<Value> {
    ensure!(
        !fs::symlink_metadata(repo)?.file_type().is_symlink(),
        "repository must not be a symlink"
    );
    let root = fs::canonicalize(repo).context("resolve repository path")?;
    ensure!(root.is_dir(), "repository must be a directory");
    let (instructions, skills, mcp, manifest_path) = agent.paths();
    let writes = plan_install(&root, agent, data_dir)?;
    let mut changed = Vec::new();
    for write in writes {
        if read_optional(&root.join(&write.relative))?.as_deref() != Some(&write.bytes) {
            atomic_write(&root, &write.relative, &write.bytes)?;
            changed.push(write.relative);
        }
    }
    Ok(
        json!({"agent": agent.as_str(), "repo": root, "instructions": instructions,
        "skills": skills, "mcp_config": mcp, "manifest": manifest_path, "changed_files": changed}),
    )
}

fn plan_install(root: &Path, agent: Agent, data_dir: Option<&Path>) -> Result<Vec<PlannedWrite>> {
    let (instructions, skills, mcp, manifest_path) = agent.paths();
    let bundled = bundled::files();
    let destinations: Vec<PathBuf> = [instructions, mcp, manifest_path]
        .into_iter()
        .map(PathBuf::from)
        .chain(bundled.iter().map(|(path, _)| Path::new(skills).join(path)))
        .collect();
    for path in &destinations {
        validate_destination(root, path)?;
    }
    let manifest = read_manifest(&root.join(manifest_path))?;
    let mut writes = Vec::new();
    let text = read_optional(&root.join(instructions))?.unwrap_or_default();
    let updated =
        managed_instructions(std::str::from_utf8(&text).context("instructions must be UTF-8")?)?;
    writes.push(PlannedWrite {
        relative: instructions.into(),
        bytes: updated.into_bytes(),
    });
    let args = mcp_args(data_dir)?;
    let original = read_optional(&root.join(mcp))?;
    writes.push(PlannedWrite {
        relative: mcp.into(),
        bytes: config::merge(agent, original.as_deref(), &args)?,
    });
    let hashes = plan_skills(root, skills, bundled, manifest.as_ref(), &mut writes)?;
    let next_manifest = Manifest {
        version: 1,
        files: hashes,
    };
    writes.push(PlannedWrite {
        relative: manifest_path.into(),
        bytes: serde_json::to_vec_pretty(&next_manifest)?,
    });
    Ok(writes)
}

fn plan_skills(
    root: &Path,
    skills: &str,
    bundled: Vec<(&str, &[u8])>,
    manifest: Option<&Manifest>,
    writes: &mut Vec<PlannedWrite>,
) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for (path, bytes) in bundled {
        let relative = Path::new(skills).join(path);
        let key = relative.to_str().context("invalid skill path")?.to_owned();
        if let Some(existing) = read_optional(&root.join(&relative))? {
            let owned = manifest
                .and_then(|m| m.files.get(&key))
                .is_some_and(|hash| *hash == digest(&existing));
            ensure!(
                existing == bytes || owned,
                "skill file {} differs from Horde's bundled or previously installed content; preserve or move your edits before rerunning init",
                relative.display()
            );
        }
        hashes.insert(key, digest(bytes));
        writes.push(PlannedWrite {
            relative,
            bytes: bytes.to_vec(),
        });
    }
    Ok(hashes)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn read_manifest(path: &Path) -> Result<Option<Manifest>> {
    read_optional(path)?
        .map(|bytes| {
            let manifest: Manifest =
                serde_json::from_slice(&bytes).context("invalid Horde installation manifest")?;
            ensure!(
                manifest.version == 1,
                "unsupported Horde installation manifest version"
            );
            Ok(manifest)
        })
        .transpose()
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn managed_instructions(original: &str) -> Result<String> {
    let begin: Vec<_> = original
        .match_indices(BEGIN)
        .map(|(index, _)| index)
        .collect();
    let end: Vec<_> = original
        .match_indices(END)
        .map(|(index, _)| index)
        .collect();
    let block = format!("{BEGIN}\n{POLICY}{END}");
    match (begin.as_slice(), end.as_slice()) {
        ([], []) => {
            let separator = if original.is_empty() || original.ends_with("\n\n") {
                ""
            } else if original.ends_with('\n') {
                "\n"
            } else {
                "\n\n"
            };
            Ok(format!("{original}{separator}{block}\n"))
        }
        ([start], [finish]) if start < finish => Ok(format!(
            "{}{block}{}",
            &original[..*start],
            &original[finish + END.len()..]
        )),
        _ => bail!(
            "malformed or duplicate Horde delegation markers; repair the managed instruction block before rerunning init"
        ),
    }
}

fn mcp_args(data_dir: Option<&Path>) -> Result<Vec<String>> {
    let mut args = Vec::new();
    if let Some(path) = data_dir {
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        args.extend([
            "--data-dir".to_owned(),
            absolute
                .to_str()
                .context("data directory must be UTF-8")?
                .to_owned(),
        ]);
    }
    args.push("mcp".to_owned());
    Ok(args)
}

fn validate_destination(root: &Path, relative: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        ensure!(
            matches!(component, Component::Normal(_)),
            "destination must stay inside the repository"
        );
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                ensure!(
                    !metadata.file_type().is_symlink(),
                    "refusing symlink destination {}",
                    current.display()
                );
                let expected_type = if index + 1 == components.len() {
                    metadata.is_file()
                } else {
                    metadata.is_dir()
                };
                ensure!(
                    expected_type,
                    "unexpected file type at {}",
                    current.display()
                );
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", current.display()));
            }
        }
    }
    Ok(())
}

fn atomic_write(root: &Path, relative: &Path, bytes: &[u8]) -> Result<()> {
    validate_destination(root, relative)?;
    let destination = root.join(relative);
    let parent = destination.parent().context("destination has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".horde-init-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        if let Ok(metadata) = fs::metadata(&destination) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        validate_destination(root, relative)?;
        fs::rename(&temporary, &destination)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.with_context(|| format!("install {}", destination.display()))
}
