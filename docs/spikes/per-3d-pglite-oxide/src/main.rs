use anyhow::{anyhow, Context, Result};
use deadpool_postgres::{Manager, Pool};
use pglite_oxide::PgliteServer;
use std::{
    env, fs,
    net::{SocketAddr, TcpListener},
    path::PathBuf,
    str::FromStr,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{net::TcpStream, process::Command, task::JoinHandle, time::timeout};
use tokio_postgres::{Client, NoTls};

#[tokio::main]
async fn main() -> Result<()> {
    if matches!(env::var("PER_3D_SCENARIO").as_deref(), Ok("pool_max_2")) {
        let temp_root = unique_temp_root();
        let _cleanup = TempDirCleanup(temp_root.clone());
        run_pool_scenario(temp_root, 2).await?;
        return Ok(());
    }

    let temp_root = unique_temp_root();
    let _cleanup = TempDirCleanup(temp_root.clone());

    if run_child_scenario("pool_max_2", Duration::from_secs(8)).await? == ChildScenario::TimedOut {
        println!("pool_max_2=HungAtSecondQuery");
    }

    println!("scenario=no_user_starting");
    let no_user_server = start_server(temp_root.join("no-user")).await?;
    let no_user_profile = PostgresProfile::from_tcp_addr_without_username(no_user_server.addr);
    match connect_standard_client(no_user_profile.url.as_str()).await {
        Ok((client, connection)) => {
            drop(client);
            let _ = timeout(Duration::from_secs(5), connection).await;
            println!("no_user_profile=ok");
        }
        Err(error) => println!("no_user_profile=failed: {error:#}"),
    }
    shutdown_server("no_user", no_user_server).await?;

    println!("scenario=standard_starting");
    let standard_server = start_server(temp_root.join("standard")).await?;
    println!("profile_url={}", standard_server.profile.url);
    prove_standard_client_contract(standard_server.profile.url.as_str()).await?;
    shutdown_server("standard", standard_server).await?;

    run_pool_scenario(temp_root.join("max-one-root"), 1).await?;

    Ok(())
}

async fn run_child_scenario(scenario: &str, duration: Duration) -> Result<ChildScenario> {
    let mut child = Command::new(env::current_exe().context("find current spike executable")?)
        .env("PER_3D_SCENARIO", scenario)
        .spawn()
        .with_context(|| format!("spawn child scenario {scenario}"))?;

    match timeout(duration, child.wait()).await {
        Ok(status) => {
            let status = status.with_context(|| format!("wait for child scenario {scenario}"))?;
            anyhow::ensure!(
                status.success(),
                "child scenario {scenario} exited with {status}"
            );
            Ok(ChildScenario::Completed)
        }
        Err(_) => {
            child
                .kill()
                .await
                .with_context(|| format!("kill timed out child scenario {scenario}"))?;
            timeout(Duration::from_secs(5), child.wait())
                .await
                .with_context(|| format!("timed out reaping killed child scenario {scenario}"))?
                .with_context(|| format!("reap killed child scenario {scenario}"))?;
            Ok(ChildScenario::TimedOut)
        }
    }
}

async fn run_pool_scenario(root: PathBuf, max_size: usize) -> Result<()> {
    println!("scenario=pool_max_{max_size}_starting");
    let server = start_server(root).await?;
    println!(
        "scenario=pool_max_{max_size}_started url={}",
        server.profile.url
    );
    let outcome = classify_pool_overlap(server.profile.url.as_str(), max_size).await?;
    println!("pool_max_{max_size}={outcome:?}");
    shutdown_server(&format!("pool_max_{max_size}"), server).await
}

async fn start_server(root: PathBuf) -> Result<RunningServer> {
    let requested_addr = free_tcp_addr()?;
    let server = PgliteServer::builder()
        .path(root)
        .tcp(requested_addr)
        .username("postgres")
        .database("postgres")
        .start()
        .context("start persistent PgliteServer")?;
    let addr = server
        .tcp_addr()
        .ok_or_else(|| anyhow!("pglite server did not expose a TCP address"))?;
    let profile = PostgresProfile::from_tcp_addr(addr, "postgres");
    wait_for_tcp(addr).await?;
    Ok(RunningServer {
        server,
        addr,
        profile,
    })
}

async fn shutdown_server(label: &str, server: RunningServer) -> Result<()> {
    let addr = server.addr;
    match server.server.shutdown() {
        Ok(()) => {
            assert_server_stopped(addr).await?;
            println!("shutdown_{label}=clean");
        }
        Err(error) => {
            println!("shutdown_{label}=failed: {error:#}");
            assert_server_stopped(addr).await?;
        }
    }
    Ok(())
}

async fn prove_standard_client_contract(profile_url: &str) -> Result<()> {
    let (client, connection) = connect_standard_client(profile_url).await?;

    client
        .batch_execute(
            "CREATE TABLE postgres_contract_values (id INT PRIMARY KEY, label TEXT NOT NULL)",
        )
        .await
        .context("create contract table")?;
    client
        .execute(
            "INSERT INTO postgres_contract_values (id, label) VALUES ($1, $2), ($3, $4)",
            &[&1_i32, &"profile-derived", &2_i32, &"pglite-oxide"],
        )
        .await
        .context("insert exact contract rows")?;
    let rows = client
        .query(
            "SELECT id, label FROM postgres_contract_values ORDER BY id",
            &[],
        )
        .await
        .context("select exact contract rows")?;
    let values = rows
        .iter()
        .map(|row| (row.get::<_, i32>("id"), row.get::<_, String>("label")))
        .collect::<Vec<_>>();
    anyhow::ensure!(
        values
            == vec![
                (1, "profile-derived".to_string()),
                (2, "pglite-oxide".to_string()),
            ],
        "selected rows did not match exact inserted values: {values:?}"
    );
    println!("standard_client=ok");

    drop(client);
    let _ = timeout(Duration::from_secs(5), connection)
        .await
        .context("standard client connection task did not close")?;

    Ok(())
}

async fn classify_pool_overlap(profile_url: &str, max_size: usize) -> Result<PoolOverlapOutcome> {
    let pool = postgres_pool(profile_url, max_size)?;
    println!("pool_max_{max_size}_opening_first_client");
    let first_client = match timeout(Duration::from_secs(2), pool.get()).await {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            println!("pool_max_{max_size}_first_client_error={error:#}");
            pool.close();
            return Ok(PoolOverlapOutcome::FailedOpeningFirstClient);
        }
        Err(error) => {
            println!("pool_max_{max_size}_first_client_timeout={error:#}");
            pool.close();
            return Ok(PoolOverlapOutcome::FailedOpeningFirstClient);
        }
    };
    println!("pool_max_{max_size}_querying_first_client");
    let row = match timeout(
        Duration::from_secs(2),
        first_client.query_one("SELECT 1::INT4", &[]),
    )
    .await
    {
        Ok(Ok(row)) => row,
        Ok(Err(error)) => {
            println!("pool_max_{max_size}_first_query_error={error:#}");
            pool.close();
            return Ok(PoolOverlapOutcome::FailedOpeningFirstClient);
        }
        Err(error) => {
            println!("pool_max_{max_size}_first_query_timeout={error:#}");
            pool.close();
            return Ok(PoolOverlapOutcome::FailedOpeningFirstClient);
        }
    };
    anyhow::ensure!(
        row.get::<_, i32>(0) == 1,
        "first pooled query returned wrong value"
    );

    println!("pool_max_{max_size}_starting_second_query");
    let mut second_query = spawn_second_pool_query(pool.clone());
    let outcome = match timeout(Duration::from_millis(500), &mut second_query).await {
        Ok(joined) => match joined.context("second pooled query task failed")? {
            Ok(value) => {
                anyhow::ensure!(value == 2, "second pooled query returned wrong value");
                PoolOverlapOutcome::CompletedWhileFirstClientOpen
            }
            Err(error) => {
                println!("pool_max_{max_size}_error={error:#}");
                PoolOverlapOutcome::FailedWhileFirstClientOpen
            }
        },
        Err(_) => {
            drop(first_client);
            let value = timeout(Duration::from_secs(5), second_query)
                .await
                .context("second pooled query stayed blocked after first client closed")?
                .context("second pooled query task failed after first client closed")??;
            anyhow::ensure!(
                value == 2,
                "second pooled query returned wrong value after unblock"
            );
            PoolOverlapOutcome::BlockedUntilFirstClientClosed
        }
    };
    pool.close();
    Ok(outcome)
}

fn spawn_second_pool_query(pool: Pool) -> JoinHandle<Result<i32>> {
    tokio::spawn(async move {
        let second_client = pool.get().await.context("open second pooled client")?;
        let row = second_client
            .query_one("SELECT 2::INT4", &[])
            .await
            .context("query through second pooled client")?;
        Ok(row.get(0))
    })
}

fn postgres_pool(profile_url: &str, max_size: usize) -> Result<Pool> {
    let pg_config = tokio_postgres::Config::from_str(profile_url)
        .with_context(|| format!("parse postgres profile URL {profile_url}"))?;
    let manager = Manager::new(pg_config, NoTls);
    Pool::builder(manager)
        .max_size(max_size)
        .build()
        .context("build Postgres pool")
}

async fn connect_standard_client(profile_url: &str) -> Result<(Client, JoinHandle<()>)> {
    let (client, connection) = timeout(
        Duration::from_secs(2),
        tokio_postgres::connect(profile_url, NoTls),
    )
    .await
    .with_context(|| format!("timed out connecting standard Postgres client to {profile_url}"))?
    .with_context(|| format!("connect standard Postgres client to {profile_url}"))?;
    let connection = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((client, connection))
}

async fn assert_server_stopped(addr: SocketAddr) -> Result<()> {
    match timeout(Duration::from_secs(1), TcpStream::connect(addr)).await {
        Ok(Ok(_)) => Err(anyhow!("pglite server still accepts TCP after shutdown")),
        Ok(Err(_)) | Err(_) => Ok(()),
    }
}

async fn wait_for_tcp(addr: SocketAddr) -> Result<()> {
    let started = timeout(Duration::from_secs(10), async {
        loop {
            if TcpStream::connect(addr).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    started.with_context(|| format!("pglite server did not accept TCP at {addr}"))
}

fn free_tcp_addr() -> Result<SocketAddr> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("reserve free TCP port")?;
    listener.local_addr().context("read reserved TCP port")
}

fn unique_temp_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "local-sandbox-postgres-contract-{}",
        unique_suffix()
    ))
}

fn unique_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_else(|_| Duration::from_secs(0))
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[derive(Debug)]
struct PostgresProfile {
    url: String,
}

struct RunningServer {
    server: PgliteServer,
    addr: SocketAddr,
    profile: PostgresProfile,
}

impl PostgresProfile {
    fn from_tcp_addr(addr: SocketAddr, username: &str) -> Self {
        Self {
            url: format!(
                "postgres://{username}@{}:{}/postgres",
                addr.ip(),
                addr.port()
            ),
        }
    }

    fn from_tcp_addr_without_username(addr: SocketAddr) -> Self {
        Self {
            url: format!("postgres://{}:{}/postgres", addr.ip(), addr.port()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PoolOverlapOutcome {
    FailedOpeningFirstClient,
    CompletedWhileFirstClientOpen,
    FailedWhileFirstClientOpen,
    BlockedUntilFirstClientClosed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ChildScenario {
    Completed,
    TimedOut,
}

struct TempDirCleanup(PathBuf);

impl Drop for TempDirCleanup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
