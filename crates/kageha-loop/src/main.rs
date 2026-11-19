use kageha_loop::discovery::rank_tools;
use kageha_loop::dispatch::plan_dispatch;
use kageha_loop::job_dependencies::plan_dependencies;
use kageha_loop::job_scheduling::select_job;
use kageha_loop::model_retry::decide_model_retry;
use kageha_loop::model_stream::assemble_tools;
use kageha_loop::{reduce, Event};
use serde::Deserialize;
use serde_json::json;
use std::io::{self, BufRead, Write};

#[derive(Deserialize)]
struct ReplayInput {
    event: Event,
    #[serde(default)]
    replay_now: f64,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    unsafe {
        let mut limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) == 0 {
            let target: libc::rlim_t = 10240;
            if limit.rlim_cur < target {
                limit.rlim_cur = if limit.rlim_max == libc::RLIM_INFINITY || limit.rlim_max >= target {
                    target
                } else {
                    limit.rlim_max
                };
                let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &limit);
            }
        }
    }

    if std::env::args().nth(1).as_deref()==Some("migrate-models") {
        let args:Vec<_>=std::env::args().skip(2).collect();
        if args.len()!=5 && !(args.len()==6&&matches!(args[5].as_str(),"--apply"|"--repair-config")) {return Err("usage: kageha-loop migrate-models WORKSPACE STATE_DIR ENV_FILE LOCAL_API_ENDPOINT TOKEN_FILE [--apply|--repair-config]".into());}
        let report=kageha_loop::model_migration::run(std::path::Path::new(&args[0]),std::path::Path::new(&args[1]),std::path::Path::new(&args[2]),&args[3],std::path::Path::new(&args[4]),args.len()==6,args.get(5).is_some_and(|v|v=="--repair-config"))?;
        println!("{report}");return Ok(());
    }
    if std::env::args().nth(1).as_deref()==Some("migrate-projects") {
        let args:Vec<_>=std::env::args_os().skip(2).collect();if args.len()!=2{return Err("usage: kageha-loop migrate-projects SOURCE_HOME STATE_DIR".into());}
        let report=kageha_loop::project_migration::run(std::path::Path::new(&args[0]),std::path::Path::new(&args[1]))?;
        println!("{}",serde_json::json!({"domain":"projects","report":report,"full_migration_complete":false}));return Ok(());
    }
    if std::env::args().nth(1).as_deref()==Some("migrate-workspace-knowledge") {
        let args:Vec<_>=std::env::args_os().skip(2).collect();if args.len()!=3{return Err("usage: kageha-loop migrate-workspace-knowledge SOURCE STATE_DIR WORKSPACE".into());}
        let report=kageha_loop::document_migration::run_workspace_knowledge(std::path::Path::new(&args[0]),std::path::Path::new(&args[1]),std::path::Path::new(&args[2]))?;
        println!("{}",serde_json::json!({"domain":"workspace-knowledge","report":report,"full_migration_complete":false}));return Ok(());
    }
    if std::env::args().nth(1).as_deref()==Some("migrate-documents") {
        let args:Vec<_>=std::env::args_os().skip(2).collect();
        if args.len()!=3{return Err("usage: kageha-loop migrate-documents agents|task-plans|agent-knowledge SOURCE STATE_DIR".into());}
        let domain=match args[0].to_str(){Some("agents")=>kageha_migration::documents::Domain::Agents,Some("task-plans")=>kageha_migration::documents::Domain::TaskPlans,Some("agent-knowledge")=>kageha_migration::documents::Domain::Knowledge,_=>return Err("Unknown document migration domain".into())};
        let report=kageha_loop::document_migration::run(std::path::Path::new(&args[1]),std::path::Path::new(&args[2]),domain)?;
        println!("{}",serde_json::json!({"domain":domain.name(),"report":report,"full_migration_complete":false}));return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("migrate-history") {
        let args: Vec<_> = std::env::args_os().skip(2).collect();
        if args.len() != 2 {
            return Err("usage: kageha-loop migrate-history SOURCE_SESSIONS STATE_DIR".into());
        }
        let report = kageha_loop::history_migration::run(
            std::path::Path::new(&args[0]),
            std::path::Path::new(&args[1]),
        )?;
        println!(
            "{}",
            serde_json::json!({"domain":"legacy-conversation-history","schema_version":1,"report":report,"full_migration_complete":false})
        );
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("recover-legacy-browser") {
        let args: Vec<_> = std::env::args().skip(2).collect();
        if args.is_empty() || args.len() > 2 || args.get(1).is_some_and(|arg| arg != "--watch") {
            return Err("usage: kageha-loop recover-legacy-browser STATE_DIR [--watch]".into());
        }
        return kageha_loop::legacy_browser_recovery::run(
            std::path::Path::new(&args[0]),
            args.len() == 2,
        );
    }
    if std::env::args().nth(1).as_deref() == Some("extract-document-stdin") {
        use std::io::Read;
        kageha_workspace::worker_limits();
        let suffix = std::env::args()
            .nth(2)
            .ok_or("document extension required")?;
        let mut bytes = Vec::new();
        std::io::stdin()
            .take((kageha_workspace::DOCUMENT_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        print!(
            "{}",
            kageha_loop::documents::extract_bytes(&bytes, &suffix)?
        );
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("extract-document") {
        let path = std::env::args_os().nth(2).ok_or("document path required")?;
        #[cfg(unix)]
        unsafe {
            let limit = libc::rlimit {
                rlim_cur: 1024 * 1024 * 1024,
                rlim_max: 1024 * 1024 * 1024,
            };
            libc::setrlimit(libc::RLIMIT_AS, &limit);
        }
        print!(
            "{}",
            kageha_loop::documents::extract(std::path::Path::new(&path))?
        );
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("serve") {
        let args: Vec<_> = std::env::args().skip(2).collect();
        if !(2..=3).contains(&args.len()) {
            return Err("usage: kageha-loop serve WORKSPACE STATE_DIR [BIND]".into());
        }
        return kageha_loop::server::run(
            std::path::Path::new(&args[0]),
            std::path::Path::new(&args[1]),
            args.get(2).map(String::as_str).unwrap_or("127.0.0.1:8081"),
        );
    }
    if std::env::args().nth(1).as_deref() == Some("chat") {
        let args: Vec<String> = std::env::args().skip(2).collect();
        if args.first().map(String::as_str) == Some("--offline") {
            if args.len() != 2 {
                return Err("usage: kageha-loop chat --offline WORKSPACE".into());
            }
            kageha_loop::native_chat::run(
                std::path::Path::new(&args[1]),
                io::stdin().lock(),
                io::stdout().lock(),
            )?;
        } else {
            kageha_session_client::cli::run(args)?;
        }
        return Ok(());
    }
    #[cfg(unix)]
    if std::env::args().nth(1).as_deref() == Some("ssh") {
        return kageha_loop::ssh::run(&std::env::args().skip(2).collect::<Vec<_>>());
    }
    if std::env::args().nth(1).as_deref() == Some("plan-dispatch") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &plan_dispatch(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("rank-tools") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &rank_tools(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("assemble-tools") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &assemble_tools(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("decide-model-retry") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &decide_model_retry(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("plan-job-dependencies") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &plan_dependencies(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("select-job") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(io::stdout().lock(), &select_job(request)?)?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("validate-plan") {
        let steps: Vec<kageha_loop::plan::Step> = serde_json::from_reader(io::stdin().lock())?;
        let error = kageha_loop::plan::validate(&steps).err();
        serde_json::to_writer(io::stdout().lock(), &json!({"version": 1, "error": error}))?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("run-turn") {
        kageha_loop::turn::run_turn(io::stdin().lock(), io::stdout().lock())?;
        return Ok(());
    }
    if std::env::args().nth(1).as_deref() == Some("control-session") {
        let request = serde_json::from_reader(io::stdin().lock())?;
        serde_json::to_writer(
            io::stdout().lock(),
            &kageha_loop::session::control(request)?,
        )?;
        return Ok(());
    }
    let mut snapshot = None;
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let input: ReplayInput = serde_json::from_str(&line?)?;
        let output = match reduce(&mut snapshot, &input.event, input.replay_now) {
            Ok(()) => json!({"snapshot": snapshot}),
            Err(error) => json!({"error": error}),
        };
        serde_json::to_writer(&mut stdout, &output)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    Ok(())
}
