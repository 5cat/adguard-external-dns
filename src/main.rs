use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    ptr::eq,
};

use futures::TryStreamExt;
use k8s_openapi::api::networking::v1::Ingress;
use kube::{
    api::{Api, ListParams},
    runtime::{watcher, watcher::Event, WatchStreamExt},
    Client,
};
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Serialize, Deserialize, Debug)]
struct Record {
    domain: String,
    answer: String,
}

impl Record {
    fn new(domain: String, answer: String) -> Self {
        Self {domain, answer}
    }
}

impl Hash for Record {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.domain.hash(state);
    }
}

impl PartialEq for Record {
    fn eq(&self, other: &Self) -> bool {
        self.domain == other.domain
    }
}

impl Eq for Record {}

struct AdGuard {
    host: String,
    use_https: bool,
}

impl AdGuard {
    fn new(host: String, use_https: bool) -> Self {
        AdGuard { host, use_https }
    }
    fn build_url(&self, endpoint: &str) -> String {
        let mut url = "http".to_owned();
        if self.use_https {
            url.push_str("s");
        }
        url.push_str("://");
        url.push_str(&self.host);
        url.push_str(endpoint);
        url
    }

    async fn add_record(&self, record: &Record) -> Result<(), reqwest::Error> {
        let response = reqwest::Client::new()
            .post(self.build_url("/control/rewrite/add"))
            .json(record)
            .send()
            .await?;
        dbg!(&response);
        response.error_for_status()?;
        Ok(())
    }

    async fn delete_record(&self, record: &Record) -> Result<(), reqwest::Error> {
        let response = reqwest::Client::new()
            .post(self.build_url("/control/rewrite/delete"))
            .json(record)
            .send()
            .await?;
        dbg!(&response);
        response.error_for_status()?;
        Ok(())
    }

    async fn get_records(&self) -> Result<HashMap<String, String>, reqwest::Error> {
        let response = reqwest::Client::new()
            .get(self.build_url("/control/rewrite/list"))
            .send()
            .await?;
        dbg!(&response);
        let data: Vec<Record> = response.json().await?;
        // &response.error_for_status()?;
        let mut res: HashMap<String, String> = HashMap::new();
        for r in data {
            res.insert(r.domain.into(), r.answer.into());
        }
        Ok(res)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adguard = AdGuard::new("localhost:8080".into(), false);
    let record = Record::new("banana.fish".into(), "168.264.76.1".into());
    adguard.add_record(&record).await?;
    dbg!(adguard.get_records().await?);
    adguard.delete_record(&record).await?;
    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    // Read pods in the configured namespace into the typed interface from k8s-openapi
    let ingress: Api<Ingress> = Api::all(client);

    watcher(ingress, ListParams::default())
        // .touched_objects()
        .try_for_each(|ing| async move {
            match &ing {
                Event::Applied(s) => println!("Added"),
                Event::Deleted(s) => println!("Deleted"),
                Event::Restarted(s) => println!("Restarted"),
            };
            println!("{:#?}", ing);
            Ok(())
        })
        .await?;
    Ok(())
}
