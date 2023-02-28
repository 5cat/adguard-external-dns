use std::{
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    ptr::eq,
};

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::networking::v1::Ingress;
use kube::{
    api::{Api, ListParams},
    runtime::{watcher, watcher::Event, WatchStreamExt},
    Client,
};
use reqwest;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Record {
    domain: String,
    answer: String,
}

impl Record {
    fn new(domain: String, answer: String) -> Self {
        Self { domain, answer }
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

#[derive(Clone, Copy)]
struct AdGuard<'a> {
    host: &'a str,
    use_https: bool,
}

impl<'a> AdGuard<'a> {
    fn new(host: &'a str, use_https: bool) -> Self {
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

struct IngressNeededInfo {
    host: String,
    ip: String,
}

fn extract_needed_info(ing: &Ingress) -> Vec<IngressNeededInfo> {
    let mut hosts = vec![];
    if let Some(ing_spec) = &ing.spec {
        if let Some(ing_rules) = &ing_spec.rules {
            // here im just returning the first rule since idc about other rules
            for ing_rule in ing_rules {
                if let Some(ing_host) = &ing_rule.host {
                    hosts.push(ing_host)
                }
            }
        }
    }
    let mut ip = None;
    if let Some(ing_status) = &ing.status {
        if let Some(ing_lb_status) = &ing_status.load_balancer {
            if let Some(ing_lb_status_ingress) = &ing_lb_status.ingress {
                assert!(ing_lb_status_ingress.len() <= 1);
                for ing_lb_status_ing in ing_lb_status_ingress {
                    ip = ing_lb_status_ing.ip.clone();
                }
            }
        }
    }
    if let Some(ip_clear) = ip {
        return hosts
            .into_iter()
            .map(|host| IngressNeededInfo {
                host: host.clone(),
                ip: ip_clear.clone(),
            })
            .collect();
    }
    vec![]
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let adguard = AdGuard::new("localhost:8080".into(), false);
    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    // Read pods in the configured namespace into the typed interface from k8s-openapi
    let ingress: Api<Ingress> = Api::all(client);

    watcher(ingress, ListParams::default())
        .try_for_each(|ing| async move {
            println!("{:#?}", &ing);
            match &ing {
                Event::Applied(s) => {
                    println!("Added");
                    for ini in extract_needed_info(s) {
                        adguard
                            .add_record(&Record::new(ini.host, ini.ip))
                            .await
                            .unwrap();
                    }
                }
                Event::Deleted(s) => {
                    println!("Deleted");
                    for ini in extract_needed_info(s) {
                        adguard
                            .delete_record(&Record::new(ini.host, ini.ip))
                            .await
                            .unwrap();
                    }
                }
                Event::Restarted(ss) => {
                    println!("Restarted");
                    let current_records = adguard.get_records().await.unwrap();
                    for s in ss {
                        for ini in extract_needed_info(s) {
                            let record = &Record::new(ini.host.clone(), ini.ip.clone());
                            if !current_records.contains_key(&ini.host) {
                                adguard.add_record(record).await.unwrap();
                            } else if current_records.get(&ini.host).unwrap().eq(&ini.ip) {
                                adguard.delete_record(record).await.unwrap();
                                adguard.add_record(record).await.unwrap();
                            }
                        }
                    }
                }
            };
            Ok(())
        })
        .await?;
    Ok(())
}
