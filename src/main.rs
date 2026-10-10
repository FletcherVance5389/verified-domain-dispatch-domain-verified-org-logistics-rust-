use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};
use verified_domain_dispatch::infrai_client::InfraiClient;
use verified_domain_dispatch::workspace_join::{join_verified_employee, JoinDecision, JoinRequest};

struct ThreadWaker;
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        std::thread::current().unpark();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker));
    let mut context = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}

fn main() {
    if let Err(error) = block_on(run()) {
        eprintln!("join failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let request = JoinRequest {
        email: required_arg(&mut args, "email")?,
        name: required_arg(&mut args, "name")?,
        company_domain: required_arg(&mut args, "company domain")?,
        workspace_id: required_arg(&mut args, "workspace id")?,
        ownership_proof: required_arg(&mut args, "TXT proof")?,
    };
    let client = InfraiClient::from_env()?;
    match join_verified_employee(&client, &request).await? {
        JoinDecision::Joined {
            user_id,
            workspace_id,
        } => println!("joined user={user_id} workspace={workspace_id}"),
        JoinDecision::AwaitingDns { domain } => {
            println!("awaiting TXT verification domain={domain}")
        }
    }
    Ok(())
}

fn required_arg(
    args: &mut impl Iterator<Item = String>,
    label: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    args.next().ok_or_else(|| {
        format!(
            "missing {label}; run: cargo run -- <email> <name> <domain> <workspace-id> <txt-proof>"
        )
        .into()
    })
}
