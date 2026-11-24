//! Rivulet engine prototype: a console front end for checking the engine
//! before the window UI exists.
//!
//!   rivulet preview <torrent|magnet|url>
//!   rivulet add <torrent|magnet|url> [--files 0,2] [--to DIR] [--alias NAME]
//!   rivulet run                     resume saved downloads and show progress
//!   rivulet list                    show saved downloads once
//!   rivulet pause|resume <id>
//!   rivulet remove <id> [--delete-files]
//!   rivulet profile [calm|steady|surge]
//!
//! Data lives in %LOCALAPPDATA%\Rivulet (or RIVULET_DATA), downloads go to
//! the Downloads folder unless --to is given.

use rivulet::engine;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use engine::{Engine, Profile, Snapshot, Status};

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        println!("{USAGE}");
        return Ok(());
    };
    let engine = Engine::start(&data_dir()?, &downloads_dir()).await?;
    let result = run(&engine, command, &args[1..]).await;
    engine.stop().await;
    result
}

const USAGE: &str = "rivulet preview|add|run|list|pause|resume|remove|profile ... (see src/main.rs)";

async fn run(engine: &Engine, command: &str, rest: &[String]) -> Result<()> {
    match command {
        "preview" => {
            let draft = engine.preview(arg(rest, 0)?).await?;
            println!("{}  ({})", draft.name, human(draft.total_size));
            println!("info hash {}", draft.info_hash);
            for f in &draft.files {
                println!("  [{:>3}] {:>10}  {}", f.index, human(f.size), f.path);
            }
        }
        "add" => {
            let source = arg(rest, 0)?;
            let files = flag(rest, "--files")
                .map(|v| {
                    v.split(',')
                        .map(|n| n.trim().parse::<usize>().context("--files takes numbers like 0,2"))
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?;
            let to = flag(rest, "--to").map(PathBuf::from);
            let alias = flag(rest, "--alias").map(str::to_owned);
            println!("reading the torrent…");
            let draft = engine.preview(source).await?;
            let id = engine.add(&draft, files, to.as_deref(), alias).await?;
            println!("added #{id}: {}", draft.name);
            watch(engine).await?;
        }
        "run" => watch(engine).await?,
        "list" => {
            // Give restored torrents a moment to report their state.
            tokio::time::sleep(Duration::from_millis(500)).await;
            for s in engine.snapshots() {
                println!("{}", line(&s));
            }
        }
        "remove" => {
            let id = arg(rest, 0)?.parse().context("id must be a number")?;
            engine.remove(id, rest.iter().any(|a| a == "--delete-files")).await?;
            println!("removed #{id}");
        }
        "pause" | "resume" => {
            let id = arg(rest, 0)?.parse().context("id must be a number")?;
            if command == "pause" {
                engine.pause(id).await?
            } else {
                engine.resume(id).await?
            }
            println!("{command}d #{id}");
        }
        "profile" => match rest.first() {
            None => println!("{:?}", engine.profile()),
            Some(p) => {
                let p = Profile::parse(p).context("profile is calm, steady or surge")?;
                engine.set_profile(p)?;
                println!("profile set to {p:?}");
            }
        },
        other => bail!("unknown command {other}\n{USAGE}"),
    }
    Ok(())
}

/// Prints progress every second until all downloads finish or Ctrl+C.
async fn watch(engine: &Engine) -> Result<()> {
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => { println!("stopping…"); return Ok(()); }
            _ = tick.tick() => {}
        }
        let list = engine.snapshots();
        for s in &list {
            println!("{}", line(s));
        }
        if !list.is_empty()
            && list.iter().all(|s| matches!(s.status, Status::Seeding | Status::Finished | Status::Error))
        {
            println!("all downloads are done");
            return Ok(());
        }
    }
}

fn line(s: &Snapshot) -> String {
    let title = s.alias.as_deref().unwrap_or(&s.name);
    let eta = s.eta_secs.map_or(String::new(), |e| format!(" eta {e}s"));
    let err = s.error.as_deref().map_or(String::new(), |e| format!(" — {e}"));
    format!(
        "#{} {:<11} {:>5.1}%  {} / {}  down {}/s up {}/s  seeds {} peers {}{}  {}{}",
        s.id,
        format!("{:?}", s.status),
        s.progress * 100.0,
        human(s.downloaded),
        human(s.size),
        human(s.down_bps),
        human(s.up_bps),
        s.seeds,
        s.peers,
        eta,
        title,
        err
    )
}

fn arg(rest: &[String], i: usize) -> Result<&str> {
    rest.get(i).filter(|a| !a.starts_with("--")).map(String::as_str).context("missing argument")
}

fn flag<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter().position(|a| a == name).and_then(|i| rest.get(i + 1)).map(String::as_str)
}

fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

fn data_dir() -> Result<PathBuf> {
    if let Some(d) = std::env::var_os("RIVULET_DATA") {
        return Ok(PathBuf::from(d));
    }
    let base = std::env::var_os("LOCALAPPDATA").context("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(base).join("Rivulet"))
}

fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")).unwrap_or_else(|| PathBuf::from("."))
}
