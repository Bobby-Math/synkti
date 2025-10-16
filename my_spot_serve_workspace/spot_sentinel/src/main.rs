use reqwest::{StatusCode,get}; 
use serde::{Serialize, Deserialize}; 
use std::time::Duration; 

const METADATA_URL: &str = "http://169.254.169.254/latest/meta-data/spot/instance-action";

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

        let response = get(METADATA_URL).await?;

        match response.status() {
            StatusCode::OK => {
                println!("Interruption Notice Found");
                let notice = response.json::<InterruptNotice>().await?;
                println!("Notice Details:{:?}",notice);
            }
            StatusCode::NOT_FOUND => {
                println!("No InterruptNotice");
            }
            other =>  { 
                println!("Unexpected Status Code, {other}"); 
            }
        }
            Ok(())

}
