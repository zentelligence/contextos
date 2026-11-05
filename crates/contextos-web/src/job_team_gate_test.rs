use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rmcp::model::{CallToolRequestParams, CallToolResponse, CallToolResult};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler, ServiceExt};
use serde_json::json;
use tracing_subscriber::fmt::MakeWriter;

use super::*;

/// A vault server served in process: its team file is absent, and its workflow listings answer only while `listing`
/// is set, so a test can make the job listing alone fail the way a dropped connection would.
struct AbsentTeamServer {
    listing: Arc<AtomicBool>,
}

impl ServerHandler for AbsentTeamServer {
    fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> impl Future<Output = Result<CallToolResponse, ErrorData>> + Send + '_ {
        let answer = match request.name.as_ref() {
            "fs_attach_file" => Ok(CallToolResult::structured_error(
                json!({ "code": "path/not-found", "message": "no such file" }),
            )),
            "fs_list_directory" if self.listing.load(Ordering::SeqCst) => {
                let path = request
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("path"))
                    .and_then(|path| path.as_str())
                    .unwrap_or_default();
                Ok(CallToolResult::structured(if path.ends_with("/jobs") {
                    json!({ "entries": [{ "name": "nightly.toml", "kind": "file" }] })
                } else {
                    json!({ "entries": [{ "name": "sync", "kind": "dir" }] })
                }))
            }
            _ => Err(ErrorData::internal_error("the connection dropped", None)),
        };
        std::future::ready(answer.map(CallToolResponse::from))
    }
}

async fn session(listing: &Arc<AtomicBool>) -> Result<McpClient, Box<dyn std::error::Error>> {
    let server = AbsentTeamServer {
        listing: Arc::clone(listing),
    };
    let (client_io, server_io) = tokio::io::duplex(65536);
    tokio::spawn(async move {
        if let Ok(running) = server.serve(server_io).await {
            let _ = running.waiting().await;
        }
    });
    Ok(McpClient::over("contextos", ().serve(client_io).await?))
}

#[derive(Clone, Default)]
struct Log(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Log {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> MakeWriter<'a> for Log {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

impl Log {
    fn lines_at(&self, level: &str) -> Vec<String> {
        let bytes = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        let marker = format!(" {level} ");
        String::from_utf8_lossy(&bytes)
            .lines()
            .filter(|line| line.contains(&marker) && line.contains("workflow/team-invalid"))
            .map(str::to_owned)
            .collect()
    }
}

#[tokio::test]
async fn a_failed_job_listing_leaves_the_noted_team_problem_unchanged_and_says_nothing()
-> Result<(), Box<dyn std::error::Error>> {
    let listing = Arc::new(AtomicBool::new(true));
    let client = session(&listing).await?;
    let memory = JobAttentionMemory::default();
    let log = Log::default();
    let _guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(log.clone())
            .with_ansi(false)
            .finish(),
    );

    let first = team_for_pass(&client, &memory, "work").await;
    assert!(matches!(first, PassTeam::Waits), "a vault with a job and no team waits");
    assert_eq!(log.lines_at("WARN").len(), 1, "the problem is reported when it starts");

    // The listing that says whether the vault has any job fails: it is an outage, and says nothing about the team.
    listing.store(false, Ordering::SeqCst);
    let during = team_for_pass(&client, &memory, "work").await;
    assert!(matches!(during, PassTeam::Waits), "the pass is skipped");
    assert!(
        log.lines_at("INFO").is_empty(),
        "an outage does not clear the problem, so nothing says it is valid again: {:?}",
        log.lines_at("INFO")
    );

    listing.store(true, Ordering::SeqCst);
    let after = team_for_pass(&client, &memory, "work").await;
    assert!(matches!(after, PassTeam::Waits));
    assert_eq!(
        log.lines_at("WARN").len(),
        1,
        "the problem never stopped, so it is not reported as starting again: {:?}",
        log.lines_at("WARN")
    );
    assert_eq!(log.lines_at("INFO").len(), 0);
    Ok(())
}
