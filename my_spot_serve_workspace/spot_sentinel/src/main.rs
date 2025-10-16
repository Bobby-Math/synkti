
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use std::time::Duration;

const METADATA_URL: &str = "http://169.254.169.254/latest/meta-data/spot/instance-action";
const TOKEN_URL: &str = "http://169.254.169.254/latest/api/token";

#[derive(Serialize, Deserialize, Debug)]
struct InterruptNotice {
    action: String,
    time: String,
}

#[tokio::main]
async fn main() {
    loop {
        println!("/Checking for notice:");
        if let Err(e) = check_for_notice().await {
            eprintln!("Error checking for notice:{e}");
            eprintln!("This is to be expected since we are not using an ec2 instance");
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn check_for_notice() -> Result<(), reqwest::Error> {
    let client = Client::new();

    // Get a session token
    let token_response = client
        .put(TOKEN_URL)
        .header("X-aws-ec2-metadata-token-ttl-seconds", "21600")
        .send()
        .await?;

    if token_response.status() != StatusCode::OK {
        println!(
            "Unexpected status code when getting token: {}",
            token_response.status()
        );
        return Ok(());
    }

    let token = token_response.text().await?;

    // Check for interruption notice
    let response = client
        .get(METADATA_URL)
        .header("X-aws-ec2-metadata-token", token)
        .send()
        .await?;

    match response.status() {
        StatusCode::OK => {
            println!("Interruption Notice Found");
            let notice = response.json::<InterruptNotice>().await?;
            println!("Notice Details:{:?}", notice);
        }
        StatusCode::NOT_FOUND => {
            println!("No InterruptNotice");
        }
        other => {
            println!("Unexpected Status Code, {other}");
        }
    }
    Ok(())
}
