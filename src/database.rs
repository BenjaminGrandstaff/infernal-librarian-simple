//! Goal: own Librarian's PostgreSQL wiring, entirely separate from
//! infernal-law's own database -- Librarian owns its data, storage, and
//! schema independently, and can be deleted and rebuilt without touching
//! kernel correctness (see this repo's README's Architecture section).

use std::env;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use r2d2_postgres::PostgresConnectionManager;
use r2d2_postgres::postgres::{Config as PostgresConfig, Error as PostgresError, NoTls};
use r2d2_postgres::r2d2::{self, Pool, PooledConnection};

const DATABASE_URL_ENV: &str = "LIBRARIAN_DATABASE_URL";
const DEFAULT_MAX_POOL_SIZE: u32 = 10;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

type PostgresPool = Pool<PostgresConnectionManager<NoTls>>;
pub type PostgresConnection = PooledConnection<PostgresConnectionManager<NoTls>>;

#[derive(Clone)]
pub struct Database {
    pool: PostgresPool,
}

impl Database {
    pub fn connect_from_env() -> Result<Self, DatabaseError> {
        let url = env::var(DATABASE_URL_ENV)
            .map_err(|_| DatabaseError::MissingEnvironment(DATABASE_URL_ENV))?;
        if url.trim().is_empty() {
            return Err(DatabaseError::EmptyUrl);
        }
        let mut postgres_config: PostgresConfig =
            url.parse().map_err(DatabaseError::InvalidPostgresConfig)?;
        postgres_config.connect_timeout(CONNECT_TIMEOUT);
        let manager = PostgresConnectionManager::new(postgres_config, NoTls);
        let pool = Pool::builder()
            .max_size(DEFAULT_MAX_POOL_SIZE)
            .min_idle(Some(1))
            .build(manager)
            .map_err(DatabaseError::Pool)?;
        let database = Self { pool };
        database.check_connection()?;
        database.migrate()?;
        Ok(database)
    }

    pub fn check_connection(&self) -> Result<(), DatabaseError> {
        let mut connection = self.pool.get().map_err(DatabaseError::Pool)?;
        connection
            .simple_query("SELECT 1")
            .map_err(DatabaseError::Query)?;
        Ok(())
    }

    fn migrate(&self) -> Result<(), DatabaseError> {
        let mut connection = self.connection()?;
        connection
            .batch_execute(concat!(
                include_str!("../migrations/0001_documents.sql"),
                "\n",
                include_str!("../migrations/0002_search.sql"),
            ))
            .map_err(DatabaseError::Query)
    }

    pub fn connection(&self) -> Result<PostgresConnection, DatabaseError> {
        self.pool.get().map_err(DatabaseError::Pool)
    }
}

#[derive(Debug)]
pub enum DatabaseError {
    EmptyUrl,
    InvalidPostgresConfig(PostgresError),
    MissingEnvironment(&'static str),
    Pool(r2d2::Error),
    Query(PostgresError),
}

impl Display for DatabaseError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyUrl => formatter.write_str("database URL cannot be empty"),
            Self::InvalidPostgresConfig(error) => {
                write!(formatter, "invalid PostgreSQL configuration: {error}")
            }
            Self::MissingEnvironment(name) => {
                write!(formatter, "required environment variable {name} is not set")
            }
            Self::Pool(error) => write!(formatter, "database pool error: {error}"),
            Self::Query(error) => write!(formatter, "database query failed: {error}"),
        }
    }
}

impl Error for DatabaseError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidPostgresConfig(error) | Self::Query(error) => Some(error),
            Self::Pool(error) => Some(error),
            Self::EmptyUrl | Self::MissingEnvironment(_) => None,
        }
    }
}
