use sqlx::SqlitePool;
use crate::sync::error::{SyncError, SyncResult};

pub struct SchemaManager {
    pool: SqlitePool,
}

impl SchemaManager {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
    
    pub async fn initialize_schema(&self) -> SyncResult<()> {
        self.create_containers_table().await?;
        self.create_network_allocations_table().await?;
        self.create_network_state_table().await?;
        self.create_process_monitors_table().await?;
        self.create_container_logs_table().await?;
        self.create_cleanup_tasks_table().await?;
        self.create_indexes().await?;
        
        tracing::info!("Database schema initialized successfully");
        Ok(())
    }
    
    async fn create_containers_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS containers (
                id TEXT PRIMARY KEY,
                name TEXT,
                image_path TEXT NOT NULL,
                command TEXT NOT NULL,
                environment TEXT, -- JSON blob
                state TEXT CHECK(state IN ('created', 'starting', 'running', 'exited', 'error')) NOT NULL,
                exit_code INTEGER,
                pid INTEGER,
                rootfs_path TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                exited_at INTEGER,
                memory_limit_mb INTEGER,
                cpu_limit_percent REAL,
                
                -- Resource configuration
                enable_network_namespace BOOLEAN NOT NULL DEFAULT 1,
                enable_pid_namespace BOOLEAN NOT NULL DEFAULT 1,
                enable_mount_namespace BOOLEAN NOT NULL DEFAULT 1,
                enable_uts_namespace BOOLEAN NOT NULL DEFAULT 1,
                enable_ipc_namespace BOOLEAN NOT NULL DEFAULT 1,
                
                -- Metadata
                updated_at INTEGER NOT NULL
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_network_allocations_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS network_allocations (
                container_id TEXT PRIMARY KEY,
                ip_address TEXT NOT NULL,
                bridge_interface TEXT,
                veth_host TEXT,
                veth_container TEXT,
                allocation_time INTEGER NOT NULL,
                setup_completed BOOLEAN DEFAULT 0,
                status TEXT CHECK(status IN ('allocated', 'active', 'cleanup_pending', 'cleaned')) NOT NULL,
                FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_network_state_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS network_state (
                key TEXT PRIMARY KEY,
                value TEXT,
                updated_at INTEGER NOT NULL
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_process_monitors_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS process_monitors (
                container_id TEXT PRIMARY KEY,
                pid INTEGER NOT NULL,
                monitor_started_at INTEGER NOT NULL,
                last_check_at INTEGER,
                status TEXT CHECK(status IN ('monitoring', 'completed', 'failed', 'aborted')) NOT NULL,
                FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_container_logs_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS container_logs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                container_id TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                level TEXT CHECK(level IN ('debug', 'info', 'warn', 'error')) NOT NULL,
                message TEXT NOT NULL,
                FOREIGN KEY(container_id) REFERENCES containers(id) ON DELETE CASCADE
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_cleanup_tasks_table(&self) -> SyncResult<()> {
        sqlx::query(r#"
            CREATE TABLE IF NOT EXISTS cleanup_tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                container_id TEXT NOT NULL,
                resource_type TEXT CHECK(resource_type IN ('rootfs', 'network', 'cgroup', 'mounts')) NOT NULL,
                resource_path TEXT NOT NULL,
                status TEXT CHECK(status IN ('pending', 'in_progress', 'completed', 'failed')) NOT NULL,
                created_at INTEGER NOT NULL,
                completed_at INTEGER,
                error_message TEXT
            )
        "#).execute(&self.pool).await?;
        
        Ok(())
    }
    
    async fn create_indexes(&self) -> SyncResult<()> {
        // Performance indexes as specified in the documentation
        let indexes = [
            "CREATE INDEX IF NOT EXISTS idx_containers_state ON containers(state)",
            "CREATE INDEX IF NOT EXISTS idx_containers_updated_at ON containers(updated_at)",
            "CREATE INDEX IF NOT EXISTS idx_network_allocations_status ON network_allocations(status)",
            "CREATE INDEX IF NOT EXISTS idx_network_allocations_ip ON network_allocations(ip_address)",
            "CREATE INDEX IF NOT EXISTS idx_process_monitors_status ON process_monitors(status)",
            "CREATE INDEX IF NOT EXISTS idx_process_monitors_pid ON process_monitors(pid)",
            "CREATE INDEX IF NOT EXISTS idx_container_logs_container_time ON container_logs(container_id, timestamp)",
            "CREATE INDEX IF NOT EXISTS idx_container_logs_level ON container_logs(level)",
            "CREATE INDEX IF NOT EXISTS idx_cleanup_tasks_status ON cleanup_tasks(status)",
            "CREATE INDEX IF NOT EXISTS idx_cleanup_tasks_container ON cleanup_tasks(container_id)",
        ];
        
        for index_sql in indexes {
            sqlx::query(index_sql).execute(&self.pool).await?;
        }
        
        Ok(())
    }
    
    pub async fn get_schema_version(&self) -> SyncResult<i64> {
        let result = sqlx::query_scalar::<_, i64>("PRAGMA user_version")
            .fetch_one(&self.pool)
            .await?;
        Ok(result)
    }
    
    pub async fn set_schema_version(&self, version: i64) -> SyncResult<()> {
        sqlx::query(&format!("PRAGMA user_version = {}", version))
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::connection::ConnectionManager;
    use tempfile::NamedTempFile;
    
    #[tokio::test]
    async fn test_schema_creation() {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();
        
        let conn_manager = ConnectionManager::new(db_path).await.unwrap();
        let schema_manager = SchemaManager::new(conn_manager.pool().clone());
        
        schema_manager.initialize_schema().await.unwrap();
        
        // Verify tables exist
        let tables: Vec<(String,)> = sqlx::query_as(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
        ).fetch_all(conn_manager.pool()).await.unwrap();
        
        let table_names: Vec<String> = tables.into_iter().map(|(name,)| name).collect();
        
        assert!(table_names.contains(&"containers".to_string()));
        assert!(table_names.contains(&"network_allocations".to_string()));
        assert!(table_names.contains(&"process_monitors".to_string()));
        
        conn_manager.close().await;
    }
} 