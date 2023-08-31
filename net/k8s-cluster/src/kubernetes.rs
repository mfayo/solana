use {
    crate::{boxed_error, ValidatorType},
    k8s_openapi::{
        api::{
            apps::v1::{ReplicaSet, ReplicaSetSpec},
            core::v1::{
                ConfigMap, ConfigMapVolumeSource, Container, EnvVar, EnvVarSource, Namespace,
                ObjectFieldSelector, PodSpec, PodTemplateSpec, Service, ServicePort, ServiceSpec,
                Volume, VolumeMount,
            },
        },
        apimachinery::pkg::apis::meta::v1::LabelSelector,
    },
    kube::{
        api::{Api, ObjectMeta, PostParams},
        Client,
    },
    log::*,
    std::{collections::BTreeMap, error::Error},
};

pub struct Kubernetes<'a> {
    client: Client,
    namespace: &'a str,
    bootstrap_validator_selector: BTreeMap<String, String>,
    standard_validator_selector: BTreeMap<String, String>,
}

impl<'a> Kubernetes<'a> {
    pub async fn new(namespace: &'a str) -> Kubernetes<'a> {
        Kubernetes {
            client: Client::try_default().await.unwrap(),
            namespace: namespace,
            bootstrap_validator_selector: BTreeMap::default(),
            standard_validator_selector: BTreeMap::default(),
        }
    }

    pub async fn create_config_map(
        &self,
        base64_content: String,
    ) -> Result<ConfigMap, kube::Error> {
        let mut metadata = ObjectMeta::default();
        metadata.name = Some("genesis-config".to_string());
        // Define the data for the ConfigMap
        let mut data = BTreeMap::<String, String>::new();
        data.insert("genesis.bin".to_string(), base64_content);
        // Create the ConfigMap object
        let config_map = ConfigMap {
            metadata,
            data: Some(data),
            ..Default::default()
        };

        let api: Api<ConfigMap> = Api::namespaced(self.client.clone(), self.namespace);
        api.create(&PostParams::default(), &config_map).await
    }

    pub async fn namespace_exists(&self) -> Result<bool, kube::Error> {
        let namespaces: Api<Namespace> = Api::all(self.client.clone());
        let namespace_list = namespaces.list(&Default::default()).await?;

        for namespace in namespace_list.items {
            match namespace.metadata.name {
                Some(ns) => {
                    if ns == self.namespace.to_string() {
                        return Ok(true);
                    }
                }
                None => (),
            }
        }
        Ok(false)
    }

    pub fn create_selector(
        &mut self,
        validator_type: &ValidatorType, // validator or bootstrap-validator
        key: &str,
        value: &str,
    ) {
        match *validator_type {
            ValidatorType::Bootstrap => {
                self.bootstrap_validator_selector.insert(
                    key.to_string(),
                    value.to_string(), // validator or bootstrap-validator
                );
            }
            ValidatorType::Standard => {
                self.standard_validator_selector.insert(
                    key.to_string(),
                    value.to_string(), // validator or bootstrap-validator
                );
            }
        }
    }

    pub fn create_bootstrap_validator_replicas_set(
        &self,
        container_name: &str,
        image_name: &str,
        num_bootstrap_validators: i32,
        config_map_name: Option<String>,
    ) -> Result<ReplicaSet, Box<dyn Error>> {
        let env_var = vec![EnvVar {
            name: "MY_POD_IP".to_string(),
            value_from: Some(EnvVarSource {
                field_ref: Some(ObjectFieldSelector {
                    field_path: "status.podIP".to_string(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        }];

        // let command = vec!["/workspace/start-bootstrap-validator.sh".to_string()];
        let command = vec!["sleep".to_string(), "3600".to_string()];

        self.create_replicas_set(
            "bootstrap-validator",
            &self.bootstrap_validator_selector,
            container_name,
            image_name,
            num_bootstrap_validators,
            env_var,
            &command,
            config_map_name,
        )
    }

    fn create_replicas_set(
        &self,
        app_name: &str,
        label_selector: &BTreeMap<String, String>,
        container_name: &str,
        image_name: &str,
        num_validators: i32,
        env_vars: Vec<EnvVar>,
        command: &Vec<String>,
        config_map_name: Option<String>,
    ) -> Result<ReplicaSet, Box<dyn Error>> {
        let config_map_name = match config_map_name {
            Some(name) => name,
            None => return Err(boxed_error!("config_map_name is None!")),
        };

        let volume = Volume {
            name: "genesis-config-volume".into(),
            config_map: Some(ConfigMapVolumeSource {
                name: Some(config_map_name.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let volume_mount = VolumeMount {
            name: "genesis-config-volume".to_string(),
            mount_path: "/home/solana/genesis".to_string(),
            ..Default::default()
        };

        // Define the pod spec
        let pod_spec = PodTemplateSpec {
            metadata: Some(ObjectMeta {
                labels: Some(label_selector.clone()),
                ..Default::default()
            }),
            spec: Some(PodSpec {
                containers: vec![Container {
                    name: container_name.to_string(),
                    image: Some(image_name.to_string()),
                    image_pull_policy: Some("Never".to_string()), // Set the image pull policy to "Never"
                    env: Some(env_vars),
                    command: Some(command.clone()),
                    volume_mounts: Some(vec![volume_mount]),
                    ..Default::default()
                }],
                volumes: Some(vec![volume]),
                ..Default::default()
            }),
            ..Default::default()
        };

        let replicas_set_spec = ReplicaSetSpec {
            replicas: Some(num_validators),
            selector: LabelSelector {
                match_labels: Some(label_selector.clone()),
                ..Default::default()
            },
            template: Some(pod_spec),
            ..Default::default()
        };

        Ok(ReplicaSet {
            metadata: ObjectMeta {
                name: Some(format!("{}-replicaset", app_name)),
                namespace: Some(self.namespace.to_string()),
                ..Default::default()
            },
            spec: Some(replicas_set_spec),
            ..Default::default()
        })
    }

    pub async fn deploy_replicas_set(
        &self,
        replica_set: &ReplicaSet,
    ) -> Result<ReplicaSet, kube::Error> {
        let api: Api<ReplicaSet> = Api::namespaced(self.client.clone(), self.namespace);
        let post_params = PostParams::default();
        info!("creating replica set!");
        // Apply the ReplicaSet
        api.create(&post_params, replica_set).await
    }

    pub fn create_bootstrap_validator_service(&self) -> Service {
        self.create_service("bootstrap-validator", &self.bootstrap_validator_selector)
    }

    fn create_service(
        &self,
        service_name: &str,
        label_selector: &BTreeMap<String, String>,
    ) -> Service {
        Service {
            metadata: ObjectMeta {
                name: Some(format!("{}-service", service_name).to_string()),
                namespace: Some(self.namespace.to_string()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                selector: Some(label_selector.clone()),
                cluster_ip: Some("None".into()),
                // cluster_ips: None,
                ports: Some(vec![
                    ServicePort {
                        port: 8899, // RPC Port
                        name: Some("rpc-port".to_string()),
                        ..Default::default()
                    },
                    ServicePort {
                        port: 8001, //Gossip Port
                        name: Some("gossip-port".to_string()),
                        ..Default::default()
                    },
                    ServicePort {
                        port: 9900, //Faucet Port
                        name: Some("faucet-port".to_string()),
                        ..Default::default()
                    },
                ]),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    pub async fn deploy_service(&self, service: &Service) -> Result<Service, kube::Error> {
        let post_params = PostParams::default();
        // Create an API instance for Services in the specified namespace
        let service_api: Api<Service> = Api::namespaced(self.client.clone(), self.namespace);

        // Create the Service object in the cluster
        service_api.create(&post_params, &service).await
    }

    pub async fn check_replica_set_ready(&self, replica_set_name: &str) -> Result<bool, kube::Error> {
        let replica_sets: Api<ReplicaSet> = Api::namespaced(self.client.clone(), self.namespace);
        let replica_set = replica_sets.get(replica_set_name).await?;

        let desired_validators = replica_set.spec.as_ref().unwrap().replicas.unwrap_or(1);
        let available_validators = replica_set
            .status
            .as_ref()
            .unwrap()
            .available_replicas
            .unwrap_or(0);

        Ok(available_validators >= desired_validators)
    }

    pub fn create_validator_replicas_set(
        &self,
        container_name: &str,
        image_name: &str,
        num_validators: i32,
        config_map_name: Option<String>,
    ) -> Result<ReplicaSet, Box<dyn Error>> {
        let env_vars = vec![
            EnvVar {
                name: "NAMESPACE".to_string(),
                value_from: Some(EnvVarSource {
                    field_ref: Some(ObjectFieldSelector {
                        field_path: "metadata.namespace".to_string(),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            EnvVar {
                name: "BOOTSTRAP_RPC_PORT".to_string(),
                value: Some(format!(
                    "bootstrap-validator-service.$(NAMESPACE).svc.cluster.local:8899"
                )),
                ..Default::default()
            },
            EnvVar {
                name: "BOOTSTRAP_GOSSIP_PORT".to_string(),
                value: Some(format!(
                    "bootstrap-validator-service.$(NAMESPACE).svc.cluster.local:8001"
                )),
                ..Default::default()
            },
            EnvVar {
                name: "BOOTSTRAP_FAUCET_PORT".to_string(),
                value: Some(format!(
                    "bootstrap-validator-service.$(NAMESPACE).svc.cluster.local:9900"
                )),
                ..Default::default()
            },
        ];

        // let command = vec!["/workspace/start-validator.sh".to_string()];
        let command = vec!["sleep".to_string(), "3600".to_string()];

        self.create_replicas_set(
            "validator",
            &self.standard_validator_selector,
            container_name,
            image_name,
            num_validators,
            env_vars,
            &command,
            config_map_name,
        )
    }

    pub fn create_validator_service(&self) -> Service {
        self.create_service("validator", &self.standard_validator_selector)
    }

    pub async fn check_service_matching_replica_set(
        &self,
        app_name: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Get the replica_set
        let replica_set_api: Api<ReplicaSet> = Api::namespaced(self.client.clone(), self.namespace);
        let replica_set = replica_set_api
            .get(format!("{}-replicaset", app_name).as_str())
            .await?;

        // Get the Service
        let service_api: Api<Service> = Api::namespaced(self.client.clone(), self.namespace);
        let service = service_api
            .get(format!("{}-service", app_name).as_str())
            .await?;

        let replica_set_labels = replica_set
            .spec
            .and_then(|spec| {
                Some(spec.selector).and_then(|selector| {
                    selector
                        .match_labels
                        .and_then(|val| val.get("app.kubernetes.io/name").cloned())
                })
            })
            .clone();

        let service_labels = service
            .spec
            .and_then(|spec| {
                spec.selector
                    .and_then(|val| val.get("app.kubernetes.io/name").cloned())
            })
            .clone();

        info!(
            "ReplicaSet, Service labels: {:?}, {:?}",
            replica_set_labels, service_labels
        );

        let are_equal = match (replica_set_labels, service_labels) {
            (Some(rs_label), Some(serv_label)) => rs_label == serv_label,
            _ => false,
        };

        if !are_equal {
            error!("ReplicaSet and Service labels are not the same!");
        }

        Ok(())
    }
}
