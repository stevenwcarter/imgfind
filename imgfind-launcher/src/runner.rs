use anyhow::{Context, Result, bail};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChildKind {
    Imgfind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildCommandSpec {
    pub(crate) kind: ChildKind,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Build the index→thumbnails plan for indexing `folder`.
///
/// `create_new` decides whether a fresh library is created *inside* `folder`
/// (`index --root`, exploiting that `--root` creates the DB in the process cwd)
/// or whether indexing walks up into an existing ancestor library (`index`).
/// Both steps run with `cwd = folder`, and thumbnails pre-generates all GUI
/// sizes so first view is instant.
pub fn plan(folder: &Path, create_new: bool) -> Vec<ChildCommandSpec> {
    let mut index_args = vec!["index".to_string()];
    if create_new {
        index_args.push("--root".to_string());
    }
    vec![
        ChildCommandSpec {
            kind: ChildKind::Imgfind,
            args: index_args,
            cwd: folder.to_path_buf(),
        },
        ChildCommandSpec {
            kind: ChildKind::Imgfind,
            args: vec!["thumbnails".into(), "--gui-sizes".into(), "--all".into()],
            cwd: folder.to_path_buf(),
        },
    ]
}

/// Spawn each spec sequentially, streaming merged stdout+stderr line-by-line to
/// `on_line`. Stops at the first non-zero exit. `RUST_LOG=info` is set on the
/// child unless already set in this process's environment, so the live `tracing`
/// progress lines reach the log pane (indicatif bars auto-hide when piped).
pub fn run_plan(specs: &[ChildCommandSpec], mut on_line: impl FnMut(String) + Send) -> Result<()> {
    for spec in specs {
        let program = match spec.kind {
            ChildKind::Imgfind => imgfind::resolve_sibling_binary("imgfind"),
        };
        on_line(format!("$ imgfind {}", spec.args.join(" ")));

        let mut cmd = Command::new(&program);
        cmd.args(&spec.args)
            .current_dir(&spec.cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if std::env::var_os("RUST_LOG").is_none() {
            cmd.env("RUST_LOG", "info");
        }

        let mut child = cmd
            .spawn()
            .with_context(|| format!("spawning {}", program.to_string_lossy()))?;

        // Drain stdout and stderr on separate threads; call on_line live as
        // lines arrive on the calling thread.  Both threads hold a Sender clone;
        // when they both finish (streams closed) all Senders drop and the
        // receive loop exits naturally.
        let stdout = child.stdout.take().context("child stdout")?;
        let stderr = child.stderr.take().context("child stderr")?;
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let tx_out = tx.clone();
        let out_thread = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let _ = tx_out.send(line);
            }
        });
        let tx_err = tx.clone();
        let err_thread = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = tx_err.send(line);
            }
        });
        // Drop the original tx so the channel closes once both reader threads finish.
        drop(tx);
        for line in rx {
            on_line(line);
        }
        let _ = out_thread.join();
        let _ = err_thread.join();

        let status = child.wait().context("waiting for child")?;
        if !status.success() {
            on_line(format!("(exited with {status})"));
            bail!("command failed: imgfind {}", spec.args.join(" "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn plan_create_new_uses_root_then_thumbnails_all() {
        let folder = Path::new("/data/photos");
        let specs = plan(folder, true);
        assert_eq!(specs.len(), 2);
        // index --root, cwd = folder
        assert_eq!(specs[0].args, vec!["index", "--root"]);
        assert_eq!(specs[0].cwd, folder);
        // thumbnails --gui-sizes --all, cwd = folder
        assert_eq!(specs[1].args, vec!["thumbnails", "--gui-sizes", "--all"]);
        assert_eq!(specs[1].cwd, folder);
    }

    #[test]
    fn plan_existing_omits_root() {
        let folder = Path::new("/data/photos/sub");
        let specs = plan(folder, false);
        assert_eq!(specs[0].args, vec!["index"]);
        assert_eq!(specs[0].cwd, folder);
        assert_eq!(specs[1].args, vec!["thumbnails", "--gui-sizes", "--all"]);
    }
}
