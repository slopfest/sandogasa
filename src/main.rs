mod bugzilla;

use bugzilla::BzClient;

const BUGZILLA_URL: &str = "https://bugzilla.redhat.com";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = BzClient::new(BUGZILLA_URL);

    let bugs = client
        .search("product=Security Response&component=vulnerability&bug_status=NEW&limit=5")
        .await?;

    for bug in &bugs {
        println!("[{}] {}", bug.id, bug.summary);
    }

    Ok(())
}
