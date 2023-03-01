use regex::Regex;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
};

use clap::Parser;

use futures::{pin_mut, StreamExt, TryStreamExt};
use k8s_openapi::{api::networking::v1::Ingress, Metadata};
use kube::{
    api::{Api, ListParams, Patch, PatchParams},
    runtime::{watcher, watcher::Event},
    Client, Error,
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

#[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
struct IngressRecord {
    host: String,
    ip: String,
}

#[derive(Debug, Clone)]
struct IngressNeededInfo {
    current: IngressRecord,
    old: Option<IngressRecord>,
}

fn extract_needed_info(ing: &Ingress) -> Vec<IngressNeededInfo> {
    let mut hosts = vec![];
    let mut old_hosts = vec![];
    let mut old_ips = vec![];
    if let Some(ing_spec) = &ing.spec {
        if let Some(ing_rules) = &ing_spec.rules {
            // here im just returning the first rule since idc about other rules
            for ing_rule in ing_rules {
                if let Some(ing_host) = &ing_rule.host {
                    hosts.push(ing_host);
                    break;
                }
            }
        }
    }
    if let Some(ing_ann) = &ing.metadata.annotations {
        if let Some(old_host) = ing_ann.get("adguard-external-dns/old-host") {
            old_hosts.push(old_host.clone());
        }
        if let Some(old_ip) = ing_ann.get("adguard-external-dns/old-ip") {
            old_ips.push(old_ip.clone());
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
    assert!(old_hosts.len() <= 1);
    assert!(old_ips.len() <= 1);
    let mut old_record: Option<IngressRecord> = None;
    if old_hosts.len() > 0 {
        old_record = Some(IngressRecord {
            host: old_hosts.get(0).unwrap().clone(),
            ip: old_ips.get(0).unwrap().clone(),
        });
    }
    if let Some(ip_clear) = ip {
        return hosts
            .into_iter()
            .map(|host| IngressNeededInfo {
                current: IngressRecord {
                    host: host.clone(),
                    ip: ip_clear.clone(),
                },
                old: old_record.clone(),
            })
            .collect();
    }
    vec![]
}

async fn update_annotations(
    ingress: &Api<Ingress>,
    ing: &Ingress,
    record: &IngressRecord,
) -> Result<(), kube::Error> {
    // Ingress::patch(ing.metadata.name.unwrap() ing.metadata.namespace.unwrap(), body, optional)
    let params = PatchParams::apply("adguard-external-dns");
    let patch = serde_json::json!({
        "metadata": {
            "annotations": {
                "adguard-external-dns/old-host": record.host,
                "adguard-external-dns/old-ip": record.ip
            }
        }
    });
    let patch = Patch::Merge(&patch);
    ingress
        .patch(&ing.metadata.name.as_ref().unwrap(), &params, &patch)
        .await?;
    Ok(())
}

#[derive(Parser, Debug)]
struct MyOptions {
    #[arg(env)]
    adguard_host: String,
    #[arg(short, long, env, default_value_t = false)]
    adguard_use_https: bool,
    #[arg(env, short, long)]
    domain_regex: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let opts = MyOptions::parse();
    dbg!(&opts);

    let adguard = AdGuard::new(&opts.adguard_host, opts.adguard_use_https);
    // Infer the runtime environment and try to create a Kubernetes Client
    let client = Client::try_default().await?;

    // Read pods in the configured namespace into the typed interface from k8s-openapi
    let ingress: Api<Ingress> = Api::all(client.clone());

    let domain_regex_string = &opts.domain_regex.unwrap_or(".*".to_owned()).clone();
    let domain_regex = Regex::new(domain_regex_string).unwrap();
    let w = watcher(ingress, ListParams::default());
    pin_mut!(w);
    while let Some(ing) = w.try_next().await? {
        // .try_for_each(|ing| async {
        let client_2 = Client::try_default().await.unwrap();
        println!("{:#?}", &ing);
        match &ing {
            Event::Applied(s) => {
                println!("Added");
                let ingress_namespaced: Api<Ingress> =
                    Api::namespaced(client_2, s.metadata.namespace.as_ref().unwrap());
                for ini in extract_needed_info(s) {
                    if !domain_regex.is_match(&ini.current.host) {
                        continue;
                    }
                    dbg!(&ini);
                    let record = Record::new(ini.current.host.clone(), ini.current.ip.clone());
                    if let Some(old_record) = ini.old {
                        if old_record != ini.current {
                            println!("deleting old record {:#?}", old_record);
                            adguard
                                .delete_record(&Record::new(
                                    old_record.host.clone(),
                                    old_record.ip.clone(),
                                ))
                                .await
                                .unwrap();
                            println!(
                                "records do not match {:#?} != {:#?}",
                                old_record, ini.current
                            );
                            adguard.add_record(&record).await.unwrap();
                        }
                    } else {
                        adguard.add_record(&record).await.unwrap();
                    }
                    update_annotations(&ingress_namespaced, &s, &ini.current)
                        .await
                        .unwrap();
                }
            }
            Event::Deleted(s) => {
                println!("Deleted");
                for ini in extract_needed_info(s) {
                    if !domain_regex.is_match(&ini.current.host) {
                        continue;
                    }
                    adguard
                        .delete_record(&Record::new(ini.current.host, ini.current.ip))
                        .await
                        .unwrap();
                }
            }
            Event::Restarted(ss) => {
                println!("Restarted");
                let current_records = adguard.get_records().await.unwrap();
                for s in ss {
                    for ini in extract_needed_info(s) {
                        if !domain_regex.is_match(&ini.current.host) {
                            continue;
                        }
                        let record = &Record::new(ini.current.host.clone(), ini.current.ip.clone());
                        if !current_records.contains_key(&ini.current.host) {
                            adguard.add_record(record).await.unwrap();
                        } else if current_records
                            .get(&ini.current.host)
                            .unwrap()
                            .eq(&ini.current.ip)
                        {
                            adguard.delete_record(record).await.unwrap();
                            adguard.add_record(record).await.unwrap();
                        }
                    }
                }
            }
        };
    }
    Ok(())
}
