use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Context;
use bollard::{
    Docker, body_full,
    errors::Error::{DockerContainerWaitError, DockerResponseServerError},
    models::NetworkCreateRequest,
    query_parameters::{
        BuildImageOptions, BuilderVersion, InspectContainerOptions, WaitContainerOptions,
    },
};
use futures::{StreamExt, TryStreamExt};
use futures_core::future::BoxFuture;
use sha2::{Digest, Sha256};
use tar::Builder;
use testcontainers::{
    ContainerAsync, GenericImage,
    core::error::Result as TestContainerResult,
    core::logs::{LogFrame, consumer::LogConsumer},
};
use uuid::Uuid;

fn repo_root() -> anyhow::Result<PathBuf> {
    let e2e_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = e2e_dir
        .parent()
        .and_then(|p| p.parent())
        .context("tests/e2e has no parent directory?")?;
    root.canonicalize().context("canonicalizing repo root")
}

pub struct BuildContext<I> {
    root: PathBuf,
    pairs: I,
}

impl<I, From, To> BuildContext<I>
where
    I: Iterator<Item = (From, To)>,
    From: AsRef<Path>,
    To: AsRef<Path>,
{
    pub fn new(root: PathBuf, pairs: I) -> Self {
        Self { root, pairs }
    }

    pub fn tar(self) -> io::Result<Vec<u8>> {
        let mut ar = Builder::new(Vec::new());

        for (from, to) in self.pairs {
            let from_abs = self.root.join(from.as_ref());
            let to = to.as_ref();

            if to.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "destination path must be relative inside the tar: {}",
                        to.display()
                    ),
                ));
            }

            let fromtype = fs::metadata(&from_abs)?.file_type(); // Ensure it exists.

            if fromtype.is_file() {
                ar.append_path_with_name(&from_abs, to)?;
            } else if fromtype.is_dir() {
                ar.append_dir_all(to, &from_abs)?;
            } else {
                eprintln!("Skipping non-file, non-directory: {}", from_abs.display());
            }
        }

        ar.into_inner()
    }
}

pub struct TestImage {
    docker: Docker,
    tag: String,
    dockerfile: String,
    context_pairs: Vec<(PathBuf, PathBuf)>,
    buildargs: HashMap<String, String>,
}

impl TestImage {
    pub fn new(docker: Docker, tag: &str) -> Self {
        Self {
            docker,
            tag: tag.into(),
            dockerfile: "tests/e2e/docker/Dockerfile".into(),
            context_pairs: vec![
                (".dockerignore".into(), ".dockerignore".into()),
                ("crates".into(), "crates".into()),
                (
                    "tests/e2e/docker/Dockerfile".into(),
                    "tests/e2e/docker/Dockerfile".into(),
                ),
                ("tests/e2e/scenarios".into(), "tests/e2e/scenarios".into()),
            ],
            buildargs: HashMap::from([(
                "MANIFEST_DIR".into(),
                "tests/e2e/scenarios".into(),
            )]),
        }
    }

    pub fn ptp4l(docker: Docker) -> Self {
        Self {
            docker,
            tag: "ptp4l".into(),
            dockerfile: "tests/e2e/docker/ptp4l.Dockerfile".into(),
            context_pairs: vec![
                (".dockerignore".into(), ".dockerignore".into()),
                (
                    "tests/e2e/docker/ptp4l.Dockerfile".into(),
                    "tests/e2e/docker/ptp4l.Dockerfile".into(),
                ),
            ],
            buildargs: HashMap::new(),
        }
    }

    pub async fn build(&self) -> anyhow::Result<String> {
        let ctx = BuildContext::new(
            repo_root()?,
            self.context_pairs.clone().into_iter(),
        );

        let ctx_tar = ctx.tar()?;

        let mut hash = Sha256::new();
        hash.update(&ctx_tar);
        let hash_str = format!("{:x}", hash.finalize());
        let short = &hash_str[..12];

        let content_tag = format!("{}-{}", self.tag, short);

        let opts = BuildImageOptions {
            dockerfile: self.dockerfile.clone(),
            t: Some(format!("{}:{}", "rptp-e2e-test", content_tag)),
            pull: Some("missing".into()),
            rm: true,
            forcerm: true,
            buildargs: Some(self.buildargs.clone()),
            labels: Some(HashMap::from([("rptp.e2e".into(), "true".into())])),
            version: BuilderVersion::BuilderBuildKit,
            session: Some(Uuid::new_v4().to_string()),
            ..Default::default()
        };

        let mut stream = self
            .docker
            .build_image(opts, None, Some(body_full(ctx_tar.into())));

        while let Some(msg) = stream.try_next().await? {
            if let Some(s) = msg.stream {
                print!("{s}");
            }
            if let Some(e) = msg.error {
                anyhow::bail!(e);
            }
        }

        Ok(content_tag)
    }

    pub fn name(&self) -> &str {
        "rptp-e2e-test"
    }
}

#[derive(Clone)]
pub struct PrintLog {
    pub prefix: &'static str,
}

impl LogConsumer for PrintLog {
    fn accept<'a>(&self, log: &'a LogFrame) -> BoxFuture<'a, ()> {
        let prefix = self.prefix;
        Box::pin(async move {
            match log {
                LogFrame::StdOut(message) => {
                    print!("[{}] {}", prefix, String::from_utf8_lossy(message));
                }
                LogFrame::StdErr(message) => {
                    eprint!("[{}] {}", prefix, String::from_utf8_lossy(message));
                }
            }
        })
    }
}

pub struct TestNetwork {
    docker: Docker,
    name: String,
}

impl TestNetwork {
    pub async fn new(docker: Docker, prefix: &str) -> anyhow::Result<Self> {
        let name = format!("{prefix}-{}", uuid::Uuid::new_v4().simple());

        let req = NetworkCreateRequest {
            name: name.clone(),
            ..Default::default()
        };

        let _ = docker.create_network(req).await?;
        Ok(Self { docker, name })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for TestNetwork {
    fn drop(&mut self) {
        let docker = self.docker.clone();
        let name = self.name.clone();
        let timeout = std::time::Duration::from_secs(15);

        let _ = std::thread::Builder::new()
            .name("rptp-testnet-cleanup".into())
            .spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(async move {
                        match tokio::time::timeout(timeout, docker.remove_network(&name)).await {
                            Ok(Ok(_)) => {}
                            Ok(Err(e)) => {
                                match e {
                                    DockerResponseServerError {
                                        status_code: 404, ..
                                    } => {
                                        // not found; already removed.
                                    }
                                    _ => {
                                        eprintln!(
                                            "[e2e] failed to remove network '{}': {}",
                                            name, e
                                        );
                                    }
                                }
                            }
                            Err(_) => {
                                eprintln!(
                                    "[e2e] timeout removing network '{}' after {:?}",
                                    name, timeout
                                );
                            }
                        }
                    });
                }
            })
            .map(|jh| {
                let _ = jh.join();
            });
    }
}

pub struct TestContainer {
    container: ContainerAsync<GenericImage>,
    docker: Docker,
}

impl TestContainer {
    pub fn new(container: ContainerAsync<GenericImage>, docker: Docker) -> Self {
        Self { container, docker }
    }

    pub async fn wait_for_exit(&self, timeout: Duration) -> anyhow::Result<i64> {
        let id = self.container.id();

        let mut stream = self.docker.wait_container(
            id,
            Some(WaitContainerOptions {
                condition: "not-running".into(),
            }),
        );

        let exit_code = match tokio::time::timeout(timeout, stream.next()).await {
            Ok(Some(Ok(resp))) => resp.status_code,
            Ok(Some(Err(DockerContainerWaitError { code, .. }))) => code,
            Ok(Some(Err(e))) => {
                eprintln!("[e2e] wait_container error for {id}: {e}. Falling back to inspect.");
                let info = self
                    .docker
                    .inspect_container(id, None::<InspectContainerOptions>)
                    .await?;
                info.state.and_then(|s| s.exit_code).unwrap_or_else(|| {
                    eprintln!("[e2e] inspect had no exit_code for {id}, defaulting to -1");
                    -1
                })
            }
            Ok(None) => anyhow::bail!("wait_container stream ended unexpectedly for {id}"),
            Err(_) => anyhow::bail!("Timed out waiting for container {id} to stop"),
        };

        Ok(exit_code)
    }

    pub async fn stop(&self) -> TestContainerResult<()> {
        self.container.stop().await
    }

    pub async fn rm(self) -> TestContainerResult<()> {
        self.container.rm().await
    }
}
