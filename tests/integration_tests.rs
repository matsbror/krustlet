use k8s_openapi::api::core::v1::{Node, Pod, Taint};
use kube::api::{Api, DeleteParams, LogParams, PostParams};
use serde_json::json;

mod assert;
mod expectations;
mod pod_builder;
mod pod_setup;
mod test_resource_manager;
use expectations::{assert_container_statuses, ContainerStatusExpectation};
use pod_builder::{
    wasmerciser_pod, WasmerciserContainerSpec, WasmerciserVolumeSource, WasmerciserVolumeSpec,
};
use pod_setup::{wait_for_pod_complete, wait_for_pod_ready, OnFailure};
use test_resource_manager::{TestResource, TestResourceManager, TestResourceSpec};

fn in_ci_environment() -> bool {
    std::env::var("KRUSTLET_TEST_ENV") == Ok("ci".to_owned())
}

#[tokio::test]
async fn test_wascc_provider() -> Result<(), Box<dyn std::error::Error>> {
    let client = kube::Client::try_default().await?;

    let nodes: Api<Node> = Api::all(client);

    let node = nodes.get("krustlet-wascc").await?;

    verify_wascc_node(node).await;

    let client: kube::Client = nodes.into();

    let _cleaner = WasccTestResourceCleaner {};

    let pods: Api<Pod> = Api::namespaced(client.clone(), "default");

    create_wascc_pod(client.clone(), &pods).await?;

    let mut tries: u8 = 0;
    loop {
        // Send a request to the pod to trigger some logging
        if reqwest::get("http://127.0.0.1:30000").await.is_ok() {
            break;
        }
        tries += 1;
        if tries == 10 {
            panic!("Wascc pod failed 10 readiness checks.");
        }
        tokio::time::delay_for(std::time::Duration::from_millis(100)).await;
    }

    let logs = pods
        .logs("greet-wascc", &LogParams::default())
        .await
        .expect("unable to get logs");
    assert!(logs.contains("warn something"));
    assert!(logs.contains("info something"));
    assert!(logs.contains("raw msg I'm a Body!"));
    assert!(logs.contains("error body"));

    Ok(())
}

async fn verify_wascc_node(node: Node) {
    let node_status = node.status.expect("node reported no status");
    assert_eq!(
        node_status
            .node_info
            .expect("node status reported no info")
            .architecture,
        "wasm-wasi",
        "expected node to support the wasm-wasi architecture"
    );

    let node_meta = node.metadata;
    assert_eq!(
        node_meta
            .labels
            .expect("node had no labels")
            .get("kubernetes.io/arch")
            .expect("node did not have kubernetes.io/arch label"),
        "wasm32-wascc"
    );

    let taints = node
        .spec
        .expect("node had no spec")
        .taints
        .expect("node had no taints");
    let taint = taints
        .iter()
        .find(|t| (t.key == "kubernetes.io/arch") & (t.effect == "NoExecute"))
        .expect("did not find kubernetes.io/arch taint");
    // There is no "operator" field in the type for the crate for some reason,
    // so we can't compare it here
    assert_eq!(
        taint,
        &Taint {
            effect: "NoExecute".to_owned(),
            key: "kubernetes.io/arch".to_owned(),
            value: Some("wasm32-wascc".to_owned()),
            ..Default::default()
        }
    );

    let taint = taints
        .iter()
        .find(|t| (t.key == "kubernetes.io/arch") & (t.effect == "NoSchedule"))
        .expect("did not find kubernetes.io/arch taint");
    // There is no "operator" field in the type for the crate for some reason,
    // so we can't compare it here
    assert_eq!(
        taint,
        &Taint {
            effect: "NoSchedule".to_owned(),
            key: "kubernetes.io/arch".to_owned(),
            value: Some("wasm32-wascc".to_owned()),
            ..Default::default()
        }
    );
}

async fn create_wascc_pod(client: kube::Client, pods: &Api<Pod>) -> anyhow::Result<()> {
    let p = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": "greet-wascc"
        },
        "spec": {
            "containers": [
                {
                    "name": "greet-wascc",
                    "image": "webassembly.azurecr.io/greet-wascc:v0.4",
                    "ports": [
                        {
                            "containerPort": 8080,
                            "hostPort": 30000
                        }
                    ],
                },
            ],
            "tolerations": [
                {
                    "effect": "NoExecute",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wascc"
                },
                {
                    "effect": "NoSchedule",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wascc"
                },
            ]
        }
    }))?;

    let pod = pods.create(&PostParams::default(), &p).await?;

    assert_eq!(pod.status.unwrap().phase.unwrap(), "Pending");

    wait_for_pod_ready(client, "greet-wascc", "default").await?;

    Ok(())
}

struct WasccTestResourceCleaner {}

impl Drop for WasccTestResourceCleaner {
    fn drop(&mut self) {
        let t = std::thread::spawn(move || {
            let mut rt =
                tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime for cleanup");
            rt.block_on(clean_up_wascc_test_resources());
        });

        t.join().expect("Failed to clean up wasCC test resources");
    }
}

async fn clean_up_wascc_test_resources() {
    let client = kube::Client::try_default()
        .await
        .expect("Failed to create client");

    let pods: Api<Pod> = Api::namespaced(client.clone(), "default");
    pods.delete("greet-wascc", &DeleteParams::default())
        .await
        .expect("Failed to delete pod");
}

async fn verify_wasi_node(node: Node) {
    let node_status = node.status.expect("node reported no status");
    assert_eq!(
        node_status
            .node_info
            .expect("node reported no information")
            .architecture,
        "wasm-wasi",
        "expected node to support the wasm-wasi architecture"
    );

    let node_meta = node.metadata;
    assert_eq!(
        node_meta
            .labels
            .expect("node had no labels")
            .get("kubernetes.io/arch")
            .expect("node did not have kubernetes.io/arch label"),
        "wasm32-wasi"
    );

    let taints = node
        .spec
        .expect("node had no spec")
        .taints
        .expect("node had no taints");
    let taint = taints
        .iter()
        .find(|t| (t.key == "kubernetes.io/arch") & (t.effect == "NoExecute"))
        .expect("did not find kubernetes.io/arch taint");
    // There is no "operator" field in the type for the crate for some reason,
    // so we can't compare it here
    assert_eq!(
        taint,
        &Taint {
            effect: "NoExecute".to_owned(),
            key: "kubernetes.io/arch".to_owned(),
            value: Some("wasm32-wasi".to_owned()),
            ..Default::default()
        }
    );
    let taint = taints
        .iter()
        .find(|t| (t.key == "kubernetes.io/arch") & (t.effect == "NoSchedule"))
        .expect("did not find kubernetes.io/arch taint");
    // There is no "operator" field in the type for the crate for some reason,
    // so we can't compare it here
    assert_eq!(
        taint,
        &Taint {
            effect: "NoSchedule".to_owned(),
            key: "kubernetes.io/arch".to_owned(),
            value: Some("wasm32-wasi".to_owned()),
            ..Default::default()
        }
    );
}

const SIMPLE_WASI_POD: &str = "hello-wasi";
const VERBOSE_WASI_POD: &str = "hello-world-verbose";
const FAILY_POD: &str = "faily-pod";
const MULTI_MOUNT_WASI_POD: &str = "multi-mount-pod";
const MULTI_ITEMS_MOUNT_WASI_POD: &str = "multi-mount-items-pod";
const LOGGY_POD: &str = "loggy-pod";
const INITY_WASI_POD: &str = "hello-wasi-with-inits";
const FAILY_INITS_POD: &str = "faily-inits-pod";
const PRIVATE_REGISTRY_POD: &str = "private-registry-pod";

async fn create_wasi_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = SIMPLE_WASI_POD;
    // Create a temp directory to use for the host path
    let tempdir = tempfile::tempdir()?;
    let p = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": pod_name
        },
        "spec": {
            "containers": [
                {
                    "name": pod_name,
                    "image": "webassembly.azurecr.io/hello-wasm:v1",
                    "volumeMounts": [
                        {
                            "mountPath": "/foo",
                            "name": "secret-test"
                        },
                        {
                            "mountPath": "/bar",
                            "name": "configmap-test"
                        },
                        {
                            "mountPath": "/baz",
                            "name": "hostpath-test"
                        }
                    ]
                },
            ],
            "tolerations": [
                {
                    "effect": "NoExecute",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wasi"
                },
                {
                    "effect": "NoSchedule",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wasi"
                },
            ],
            "nodeSelector": {
                "kubernetes.io/arch": "wasm32-wasi"
            },
            "volumes": [
                {
                    "name": "secret-test",
                    "secret": {
                        "secretName": "hello-wasi-secret"
                    }
                },
                {
                    "name": "configmap-test",
                    "configMap": {
                        "name": "hello-wasi-configmap"
                    }
                },
                {
                    "name": "hostpath-test",
                    "hostPath": {
                        "path": tempdir.path()
                    }
                }
            ]
        }
    }))?;

    let pod = pods.create(&PostParams::default(), &p).await?;
    resource_manager.push(TestResource::Pod(pod_name.to_owned()));

    assert_eq!(pod.status.unwrap().phase.unwrap(), "Pending");

    wait_for_pod_complete(
        client,
        pod_name,
        resource_manager.namespace(),
        OnFailure::Panic,
    )
    .await
}

async fn create_fancy_schmancy_wasi_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = VERBOSE_WASI_POD;
    let p = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": pod_name
        },
        "spec": {
            "containers": [
                {
                    "name": pod_name,
                    "image": "webassembly.azurecr.io/hello-world-wasi-rust:v0.1.0",
                    "args": [ "arg1", "arg2" ],
                },
            ],
            "tolerations": [
                {
                    "effect": "NoExecute",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wasi"
                },
                {
                    "effect": "NoSchedule",
                    "key": "kubernetes.io/arch",
                    "operator": "Equal",
                    "value": "wasm32-wasi"
                },
            ],
            "nodeSelector": {
                "kubernetes.io/arch": "wasm32-wasi"
            }
        }
    }))?;

    let pod = pods.create(&PostParams::default(), &p).await?;
    resource_manager.push(TestResource::Pod(pod_name.to_owned()));

    assert_eq!(pod.status.unwrap().phase.unwrap(), "Pending");

    wait_for_pod_complete(
        client,
        pod_name,
        resource_manager.namespace(),
        OnFailure::Panic,
    )
    .await
}

async fn create_multi_mount_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = MULTI_MOUNT_WASI_POD;

    let containers = vec![WasmerciserContainerSpec::named("multimount").with_args(&[
        "assert_exists(file:/mcm/mcm1)",
        "assert_exists(file:/mcm/mcm2)",
        "assert_exists(file:/mcm/mcm5)",
        "assert_exists(file:/ms/ms1)",
        "assert_exists(file:/ms/ms2)",
        "assert_exists(file:/ms/ms3)",
        "read(file:/mcm/mcm1)to(var:mcm1)",
        "read(file:/mcm/mcm5)to(var:mcm5)",
        "read(file:/ms/ms1)to(var:ms1)",
        "read(file:/ms/ms3)to(var:ms3)",
        "write(var:mcm1)to(stm:stdout)",
        "write(var:mcm5)to(stm:stdout)",
        "write(var:ms1)to(stm:stdout)",
        "write(var:ms3)to(stm:stdout)",
    ])];

    let volumes = vec![
        WasmerciserVolumeSpec {
            volume_name: "multicm",
            mount_path: "/mcm",
            source: WasmerciserVolumeSource::ConfigMap("multi-configmap"),
        },
        WasmerciserVolumeSpec {
            volume_name: "multisecret",
            mount_path: "/ms",
            source: WasmerciserVolumeSource::Secret("multi-secret"),
        },
    ];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        vec![],
        containers,
        volumes,
        OnFailure::Panic,
        resource_manager,
    )
    .await
}

async fn create_multi_items_mount_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = MULTI_ITEMS_MOUNT_WASI_POD;

    let containers = vec![WasmerciserContainerSpec::named("multimount").with_args(&[
        "assert_exists(file:/mcm/mcm1)",
        "assert_not_exists(file:/mcm/mcm2)",
        "assert_exists(file:/mcm/mcm-five)",
        "assert_exists(file:/ms/ms1)",
        "assert_not_exists(file:/ms/ms2)",
        "assert_exists(file:/ms/ms-three)",
        "read(file:/mcm/mcm1)to(var:mcm1)",
        "read(file:/mcm/mcm-five)to(var:mcm5)",
        "read(file:/ms/ms1)to(var:ms1)",
        "read(file:/ms/ms-three)to(var:ms3)",
        "write(var:mcm1)to(stm:stdout)",
        "write(var:mcm5)to(stm:stdout)",
        "write(var:ms1)to(stm:stdout)",
        "write(var:ms3)to(stm:stdout)",
    ])];

    let volumes = vec![
        WasmerciserVolumeSpec {
            volume_name: "multicm",
            mount_path: "/mcm",
            source: WasmerciserVolumeSource::ConfigMapItems(
                "multi-configmap",
                vec![("mcm1", "mcm1"), ("mcm5", "mcm-five")],
            ),
        },
        WasmerciserVolumeSpec {
            volume_name: "multisecret",
            mount_path: "/ms",
            source: WasmerciserVolumeSource::SecretItems(
                "multi-secret",
                vec![("ms1", "ms1"), ("ms3", "ms-three")],
            ),
        },
    ];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        vec![],
        containers,
        volumes,
        OnFailure::Panic,
        resource_manager,
    )
    .await
}

async fn create_loggy_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = LOGGY_POD;

    let containers = vec![
        WasmerciserContainerSpec::named("floofycat").with_args(&["write(lit:slats)to(stm:stdout)"]),
        WasmerciserContainerSpec::named("neatcat").with_args(&["write(lit:kiki)to(stm:stdout)"]),
    ];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        vec![],
        containers,
        vec![],
        OnFailure::Panic,
        resource_manager,
    )
    .await
}

async fn create_faily_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = FAILY_POD;

    let containers = vec![WasmerciserContainerSpec::named(pod_name)
        .with_args(&["assert_exists(file:/nope.nope.nope.txt)"])];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        vec![],
        containers,
        vec![],
        OnFailure::Accept,
        resource_manager,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn wasmercise_wasi(
    pod_name: &str,
    client: kube::Client,
    pods: &Api<Pod>,
    inits: Vec<WasmerciserContainerSpec>,
    containers: Vec<WasmerciserContainerSpec>,
    test_volumes: Vec<WasmerciserVolumeSpec>,
    on_failure: OnFailure,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let p = wasmerciser_pod(pod_name, inits, containers, test_volumes, "wasm32-wasi")?;

    let pod = pods.create(&PostParams::default(), &p.pod).await?;
    resource_manager.push(TestResource::Pod(pod_name.to_owned()));

    assert_eq!(pod.status.unwrap().phase.unwrap(), "Pending");

    wait_for_pod_complete(client, pod_name, resource_manager.namespace(), on_failure).await
}

async fn create_pod_with_init_containers(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = INITY_WASI_POD;

    let inits = vec![
        WasmerciserContainerSpec::named("init-1")
            .with_args(&["write(lit:slats)to(file:/hp/floofycat.txt)"]),
        WasmerciserContainerSpec::named("init-2")
            .with_args(&["write(lit:kiki)to(file:/hp/neatcat.txt)"]),
    ];

    let containers = vec![WasmerciserContainerSpec::named(pod_name).with_args(&[
        "assert_exists(file:/hp/floofycat.txt)",
        "assert_exists(file:/hp/neatcat.txt)",
        "read(file:/hp/floofycat.txt)to(var:fcat)",
        "assert_value(var:fcat)is(lit:slats)",
        "write(var:fcat)to(stm:stdout)",
    ])];

    let volumes = vec![WasmerciserVolumeSpec {
        volume_name: "hostpath-test",
        mount_path: "/hp",
        source: WasmerciserVolumeSource::HostPath,
    }];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        inits,
        containers,
        volumes,
        OnFailure::Panic,
        resource_manager,
    )
    .await
}

async fn create_pod_with_failing_init_container(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = FAILY_INITS_POD;

    let inits = vec![
        WasmerciserContainerSpec::named("init-that-fails")
            .with_args(&["assert_exists(file:/nope.nope.nope.txt)"]),
        WasmerciserContainerSpec::named("init-that-would-succeed-if-it-ran")
            .with_args(&["write(lit:slats)to(stm:stdout)"]),
    ];

    let containers = vec![WasmerciserContainerSpec::named(pod_name)
        .with_args(&["assert_exists(file:/also.nope.txt)"])];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        inits,
        containers,
        vec![],
        OnFailure::Accept,
        resource_manager,
    )
    .await
}

async fn create_private_registry_pod(
    client: kube::Client,
    pods: &Api<Pod>,
    resource_manager: &mut TestResourceManager,
) -> anyhow::Result<()> {
    let pod_name = PRIVATE_REGISTRY_POD;

    let containers = vec![
        WasmerciserContainerSpec::named("floofycat")
            .with_args(&["write(lit:slats)to(stm:stdout)"])
            .private(),
        WasmerciserContainerSpec::named("neatcat")
            .with_args(&["write(lit:kiki)to(stm:stdout)"])
            .private(),
    ];

    wasmercise_wasi(
        pod_name,
        client,
        pods,
        vec![],
        containers,
        vec![],
        OnFailure::Panic,
        resource_manager,
    )
    .await
}

async fn set_up_test(
    test_ns: &str,
) -> anyhow::Result<(kube::Client, Api<Pod>, TestResourceManager)> {
    let client = kube::Client::try_default().await?;
    let pods: Api<Pod> = Api::namespaced(client.clone(), test_ns);
    let resource_manager = TestResourceManager::initialise(test_ns, client.clone()).await?;
    Ok((client, pods, resource_manager))
}

#[tokio::test]
async fn test_wasi_node_should_verify() -> anyhow::Result<()> {
    let client = kube::Client::try_default().await?;
    let nodes: Api<Node> = Api::all(client);
    let node = nodes.get("krustlet-wasi").await?;

    verify_wasi_node(node).await;

    Ok(())
}

#[tokio::test]
async fn test_pod_logs_and_mounts() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-pod-logs-and-mounts";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    resource_manager
        .set_up_resources(vec![
            TestResourceSpec::secret("hello-wasi-secret", "myval", "a cool secret"),
            TestResourceSpec::config_map("hello-wasi-configmap", "myval", "a cool configmap"),
        ])
        .await?;

    create_wasi_pod(client.clone(), &pods, &mut resource_manager).await?;

    assert::pod_log_equals(&pods, SIMPLE_WASI_POD, "Hello, world!\n").await?;

    assert::pod_exited_successfully(&pods, SIMPLE_WASI_POD).await?;

    assert::container_file_contains(
        SIMPLE_WASI_POD,
        resource_manager.namespace(),
        "secret-test/myval",
        "a cool secret",
        "unable to open secret file",
    )
    .await?;
    assert::container_file_contains(
        SIMPLE_WASI_POD,
        resource_manager.namespace(),
        "configmap-test/myval",
        "a cool configmap",
        "unable to open configmap file",
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_can_mount_multi_values() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-can-mount-multi-values";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    resource_manager
        .set_up_resources(vec![
            TestResourceSpec::secret_multi(
                "multi-secret",
                &[
                    ("ms1", "tell nobody"),
                    ("ms2", "but the password is"),
                    ("ms3", "wait was that a foot-- aargh!!!"),
                ],
            ),
            TestResourceSpec::config_map_multi(
                "multi-configmap",
                &[
                    ("mcm1", "value1"),
                    ("mcm2", "value two"),
                    ("mcm5", "VALUE NUMBER FIVE"),
                ],
            ),
        ])
        .await?;

    create_multi_mount_pod(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_exited_successfully(&pods, MULTI_MOUNT_WASI_POD).await?;

    assert::pod_log_contains(&pods, MULTI_MOUNT_WASI_POD, "value1").await?;
    assert::pod_log_contains(&pods, MULTI_MOUNT_WASI_POD, "VALUE NUMBER FIVE").await?;

    assert::pod_log_contains(&pods, MULTI_MOUNT_WASI_POD, "tell nobody").await?;
    assert::pod_log_contains(&pods, MULTI_MOUNT_WASI_POD, "was that a foot-- aargh!!!").await?;

    Ok(())
}

#[tokio::test]
async fn test_can_mount_individual_values() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-can-mount-individual-values";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    resource_manager
        .set_up_resources(vec![
            TestResourceSpec::secret_multi(
                "multi-secret",
                &[
                    ("ms1", "tell nobody"),
                    ("ms2", "but the password is"),
                    ("ms3", "wait was that a foot-- aargh!!!"),
                ],
            ),
            TestResourceSpec::config_map_multi(
                "multi-configmap",
                &[
                    ("mcm1", "value1"),
                    ("mcm2", "value two"),
                    ("mcm5", "VALUE NUMBER FIVE"),
                ],
            ),
        ])
        .await?;

    create_multi_items_mount_pod(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_exited_successfully(&pods, MULTI_ITEMS_MOUNT_WASI_POD).await?;

    assert::pod_log_contains(&pods, MULTI_ITEMS_MOUNT_WASI_POD, "value1").await?;
    assert::pod_log_contains(&pods, MULTI_ITEMS_MOUNT_WASI_POD, "VALUE NUMBER FIVE").await?;

    assert::pod_log_contains(&pods, MULTI_ITEMS_MOUNT_WASI_POD, "tell nobody").await?;
    assert::pod_log_contains(
        &pods,
        MULTI_ITEMS_MOUNT_WASI_POD,
        "was that a foot-- aargh!!!",
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_container_args() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-container-args";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_fancy_schmancy_wasi_pod(client.clone(), &pods, &mut resource_manager).await?;

    assert::pod_log_contains(&pods, VERBOSE_WASI_POD, r#"Args are: ["arg1", "arg2"]"#).await?;

    Ok(())
}

#[tokio::test]
async fn test_container_logging() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-container-logging";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_loggy_pod(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_container_log_contains(&pods, LOGGY_POD, "floofycat", r#"slats"#).await?;
    assert::pod_container_log_contains(&pods, LOGGY_POD, "neatcat", r#"kiki"#).await?;

    Ok(())
}

#[tokio::test]
async fn test_module_exiting_with_error() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-module-exit-error";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_faily_pod(client.clone(), &pods, &mut resource_manager).await?;
    assert::main_container_exited_with_failure(&pods, FAILY_POD).await?;
    assert::pod_log_contains(
        &pods,
        FAILY_POD,
        r#"ERR: Failed with File /nope.nope.nope.txt was expected to exist but did not"#,
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_init_containers() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-init-containers";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_pod_with_init_containers(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_log_contains(&pods, INITY_WASI_POD, r#"slats"#).await?;
    assert_container_statuses(
        &pods,
        INITY_WASI_POD,
        vec![
            ContainerStatusExpectation::InitTerminated("init-1", "Module run completed"),
            ContainerStatusExpectation::InitTerminated("init-2", "Module run completed"),
            ContainerStatusExpectation::InitNotPresent(INITY_WASI_POD),
            ContainerStatusExpectation::AppNotPresent("init-1"),
            ContainerStatusExpectation::AppNotPresent("init-2"),
            ContainerStatusExpectation::AppTerminated(INITY_WASI_POD, "Module run completed"),
        ],
    )
    .await?;

    Ok(())
}

#[tokio::test]
async fn test_failing_init_containers() -> anyhow::Result<()> {
    let test_ns = "wasi-e2e-failing-init-containers";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_pod_with_failing_init_container(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_exited_with_failure(&pods, FAILY_INITS_POD).await?;
    assert::pod_reason_contains(
        &pods,
        FAILY_INITS_POD,
        "Init container init-that-fails failed",
    )
    .await?;
    assert::pod_container_log_contains(
        &pods,
        FAILY_INITS_POD,
        "init-that-fails",
        r#"ERR: Failed with File /nope.nope.nope.txt was expected to exist but did not"#,
    )
    .await?;
    // TODO: needs moar container?
    // assert_pod_log_does_not_contain(&pods, FAILY_INITS_POD, "slats").await?;
    // assert_pod_log_does_not_contain(&pods, FAILY_INITS_POD, "also.nope.txt").await?;

    Ok(())
}

#[tokio::test]
async fn test_pull_from_private_registry() -> anyhow::Result<()> {
    if !in_ci_environment() {
        return Ok(());
    }

    let test_ns = "wasi-e2e-private-registry";
    let (client, pods, mut resource_manager) = set_up_test(test_ns).await?;

    create_private_registry_pod(client.clone(), &pods, &mut resource_manager).await?;
    assert::pod_container_log_contains(&pods, PRIVATE_REGISTRY_POD, "floofycat", r#"slats"#)
        .await?;
    assert::pod_container_log_contains(&pods, PRIVATE_REGISTRY_POD, "neatcat", r#"kiki"#).await?;

    Ok(())
}
