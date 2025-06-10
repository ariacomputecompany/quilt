use serde::{Deserialize, Serialize};
use sqlx::{SqlitePool, Row};
use std::net::Ipv4Addr;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::sync::error::{SyncError, SyncResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum NetworkStatus {
    Allocated,
    Active,
    CleanupPending,
    Cleaned,
}

impl NetworkStatus {
    pub fn to_string(&self) -> String {
        match self {
            NetworkStatus::Allocated => "allocated".to_string(),
            NetworkStatus::Active => "active".to_string(),
            NetworkStatus::CleanupPending => "cleanup_pending".to_string(),
            NetworkStatus::Cleaned => "cleaned".to_string(),
        }
    }
    
    pub fn from_string(s: &str) -> SyncResult<Self> {
        match s {
            "allocated" => Ok(NetworkStatus::Allocated),
            "active" => Ok(NetworkStatus::Active),
            "cleanup_pending" => Ok(NetworkStatus::CleanupPending),
            "cleaned" => Ok(NetworkStatus::Cleaned),
            _ => Err(SyncError::ValidationFailed {
                message: format!("Invalid network status: {}", s),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub container_id: String,
    pub ip_address: String,
    pub bridge_interface: Option<String>,
    pub veth_host: Option<String>,
    pub veth_container: Option<String>,
    pub setup_required: bool,
}

#[derive(Debug, Clone)]
pub struct NetworkAllocation {
    pub container_id: String,
    pub ip_address: String,
    pub bridge_interface: Option<String>,
    pub veth_host: Option<String>,
    pub veth_container: Option<String>,
    pub allocation_time: i64,
    pub setup_completed: bool,
    pub status: NetworkStatus,
}

pub struct NetworkManager {
    pool: SqlitePool,
    ip_range_start: Ipv4Addr,
    ip_range_end: Ipv4Addr,
}

impl NetworkManager {
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            pool,
            ip_range_start: Ipv4Addr::new(172, 16, 0, 10),
            ip_range_end: Ipv4Addr::new(172, 16, 0, 250),
        }
    }
    
    pub fn with_ip_range(pool: SqlitePool, start: Ipv4Addr, end: Ipv4Addr) -> Self {
        Self {
            pool,
            ip_range_start: start,
            ip_range_end: end,
        }
    }
    
    pub async fn allocate_network(&self, container_id: &str) -> SyncResult<NetworkConfig> {
        // Check if already allocated
        if let Ok(existing) = self.get_network_allocation(container_id).await {
            tracing::debug!("Container {} already has network allocation: {}", container_id, existing.ip_address);
            return Ok(NetworkConfig {
                container_id: container_id.to_string(),
                ip_address: existing.ip_address,
                bridge_interface: existing.bridge_interface,
                veth_host: existing.veth_host,
                veth_container: existing.veth_container,
                setup_required: !existing.setup_completed,
            });
        }
        
        let ip = self.find_available_ip().await?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        
        sqlx::query(r#"
            INSERT INTO network_allocations (
                container_id, ip_address, allocation_time, setup_completed, status
            ) VALUES (?, ?, ?, ?, ?)
        "#)
        .bind(container_id)
        .bind(&ip)
        .bind(now)
        .bind(false)
        .bind(NetworkStatus::Allocated.to_string())
        .execute(&self.pool)
        .await?;
        
        tracing::info!("Allocated IP {} for container {}", ip, container_id);
        
        Ok(NetworkConfig {
            container_id: container_id.to_string(),
            ip_address: ip,
            bridge_interface: None,
            veth_host: None,
            veth_container: None,
            setup_required: true,
        })
    }
    
    pub async fn mark_network_disabled(&self, container_id: &str) -> SyncResult<()> {
        // For containers with networking disabled, we don't allocate IPs
        // This is tracked by the absence of entries in network_allocations table
        tracing::debug!("Container {} marked with networking disabled", container_id);
        Ok(())
    }
    
    pub async fn should_setup_network(&self, container_id: &str) -> SyncResult<bool> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM network_allocations WHERE container_id = ? AND status = 'allocated'"
        )
        .bind(container_id)
        .fetch_one(&self.pool)
        .await?;
        
        Ok(count > 0)
    }
    
    pub async fn mark_network_setup_complete(&self, container_id: &str, bridge_interface: &str, veth_host: &str, veth_container: &str) -> SyncResult<()> {
        let result = sqlx::query(r#"
            UPDATE network_allocations 
            SET setup_completed = ?, status = ?, bridge_interface = ?, veth_host = ?, veth_container = ?
            WHERE container_id = ?
        "#)
        .bind(true)
        .bind(NetworkStatus::Active.to_string())
        .bind(bridge_interface)
        .bind(veth_host)
        .bind(veth_container)
        .bind(container_id)
        .execute(&self.pool)
        .await?;
        
        if result.rows_affected() == 0 {
            return Err(SyncError::NotFound {
                container_id: container_id.to_string(),
            });
        }
        
        tracing::info!("Marked network setup complete for container {}", container_id);
        Ok(())
    }
    
    pub async fn get_network_allocation(&self, container_id: &str) -> SyncResult<NetworkAllocation> {
        let row = sqlx::query(r#"
            SELECT container_id, ip_address, bridge_interface, veth_host, veth_container,
                   allocation_time, setup_completed, status
            FROM network_allocations WHERE container_id = ?
        "#)
        .bind(container_id)
        .fetch_optional(&self.pool)
        .await?;
        
        match row {
            Some(row) => {
                let status_str: String = row.get("status");
                let status = NetworkStatus::from_string(&status_str)?;
                
                Ok(NetworkAllocation {
                    container_id: row.get("container_id"),
                    ip_address: row.get("ip_address"),
                    bridge_interface: row.get("bridge_interface"),
                    veth_host: row.get("veth_host"),
                    veth_container: row.get("veth_container"),
                    allocation_time: row.get("allocation_time"),
                    setup_completed: row.get("setup_completed"),
                    status,
                })
            }
            None => Err(SyncError::NotFound {
                container_id: container_id.to_string(),
            }),
        }
    }
    
    pub async fn mark_network_cleanup_pending(&self, container_id: &str) -> SyncResult<()> {
        let result = sqlx::query("UPDATE network_allocations SET status = ? WHERE container_id = ?")
            .bind(NetworkStatus::CleanupPending.to_string())
            .bind(container_id)
            .execute(&self.pool)
            .await?;
        
        if result.rows_affected() == 0 {
            return Err(SyncError::NotFound {
                container_id: container_id.to_string(),
            });
        }
        
        Ok(())
    }
    
    pub async fn mark_network_cleaned(&self, container_id: &str) -> SyncResult<()> {
        let result = sqlx::query("UPDATE network_allocations SET status = ? WHERE container_id = ?")
            .bind(NetworkStatus::Cleaned.to_string())
            .bind(container_id)
            .execute(&self.pool)
            .await?;
        
        if result.rows_affected() == 0 {
            return Err(SyncError::NotFound {
                container_id: container_id.to_string(),
            });
        }
        
        tracing::info!("Marked network cleaned for container {}", container_id);
        Ok(())
    }
    
    pub async fn delete_network_allocation(&self, container_id: &str) -> SyncResult<()> {
        let result = sqlx::query("DELETE FROM network_allocations WHERE container_id = ?")
            .bind(container_id)
            .execute(&self.pool)
            .await?;
        
        if result.rows_affected() == 0 {
            return Err(SyncError::NotFound {
                container_id: container_id.to_string(),
            });
        }
        
        tracing::info!("Deleted network allocation for container {}", container_id);
        Ok(())
    }
    
    pub async fn list_allocations(&self, status_filter: Option<NetworkStatus>) -> SyncResult<Vec<NetworkAllocation>> {
        let mut query = "
            SELECT container_id, ip_address, bridge_interface, veth_host, veth_container,
                   allocation_time, setup_completed, status
            FROM network_allocations
        ".to_string();
        
        if let Some(status) = status_filter {
            query.push_str(&format!(" WHERE status = '{}'", status.to_string()));
        }
        
        query.push_str(" ORDER BY allocation_time ASC");
        
        let rows = sqlx::query(&query).fetch_all(&self.pool).await?;
        
        let mut allocations = Vec::new();
        for row in rows {
            let status_str: String = row.get("status");
            let status = NetworkStatus::from_string(&status_str)?;
            
            allocations.push(NetworkAllocation {
                container_id: row.get("container_id"),
                ip_address: row.get("ip_address"),
                bridge_interface: row.get("bridge_interface"),
                veth_host: row.get("veth_host"),
                veth_container: row.get("veth_container"),
                allocation_time: row.get("allocation_time"),
                setup_completed: row.get("setup_completed"),
                status,
            });
        }
        
        Ok(allocations)
    }
    
    pub async fn get_networks_needing_cleanup(&self) -> SyncResult<Vec<NetworkAllocation>> {
        self.list_allocations(Some(NetworkStatus::CleanupPending)).await
    }
    
    async fn find_available_ip(&self) -> SyncResult<String> {
        // Get all allocated IPs
        let allocated_ips: Vec<(String,)> = sqlx::query_as(
            "SELECT ip_address FROM network_allocations WHERE status != 'cleaned'"
        ).fetch_all(&self.pool).await?;
        
        let allocated_set: std::collections::HashSet<String> = allocated_ips
            .into_iter()
            .map(|(ip,)| ip)
            .collect();
        
        // Find first available IP in range
        let start_int = u32::from(self.ip_range_start);
        let end_int = u32::from(self.ip_range_end);
        
        for ip_int in start_int..=end_int {
            let ip = Ipv4Addr::from(ip_int);
            let ip_str = ip.to_string();
            
            if !allocated_set.contains(&ip_str) {
                return Ok(ip_str);
            }
        }
        
        Err(SyncError::NoAvailableIp)
    }
    
    pub async fn set_network_state(&self, key: &str, value: &str) -> SyncResult<()> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64;
        
        sqlx::query(r#"
            INSERT OR REPLACE INTO network_state (key, value, updated_at)
            VALUES (?, ?, ?)
        "#)
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        
        Ok(())
    }
    
    pub async fn get_network_state(&self, key: &str) -> SyncResult<Option<String>> {
        let value: Option<String> = sqlx::query_scalar("SELECT value FROM network_state WHERE key = ?")
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;
        
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::connection::ConnectionManager;
    use crate::sync::schema::SchemaManager;
    use tempfile::NamedTempFile;
    
    async fn setup_test_db() -> (ConnectionManager, NetworkManager) {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();
        
        let conn_manager = ConnectionManager::new(db_path).await.unwrap();
        let schema_manager = SchemaManager::new(conn_manager.pool().clone());
        schema_manager.initialize_schema().await.unwrap();
        
        let network_manager = NetworkManager::new(conn_manager.pool().clone());
        
        (conn_manager, network_manager)
    }
    
    #[tokio::test]
    async fn test_network_allocation() {
        let (_conn, network_manager) = setup_test_db().await;
        
        let config = network_manager.allocate_network("test-container").await.unwrap();
        assert_eq!(config.container_id, "test-container");
        assert!(!config.ip_address.is_empty());
        assert!(config.setup_required);
        
        // Verify allocation persisted
        let allocation = network_manager.get_network_allocation("test-container").await.unwrap();
        assert_eq!(allocation.ip_address, config.ip_address);
        assert_eq!(allocation.status, NetworkStatus::Allocated);
        assert!(!allocation.setup_completed);
    }
    
    #[tokio::test]
    async fn test_network_setup_completion() {
        let (_conn, network_manager) = setup_test_db().await;
        
        let config = network_manager.allocate_network("test-container").await.unwrap();
        
        network_manager.mark_network_setup_complete(
            "test-container",
            "br0",
            "veth123",
            "eth0"
        ).await.unwrap();
        
        let allocation = network_manager.get_network_allocation("test-container").await.unwrap();
        assert_eq!(allocation.status, NetworkStatus::Active);
        assert!(allocation.setup_completed);
        assert_eq!(allocation.bridge_interface, Some("br0".to_string()));
        assert_eq!(allocation.veth_host, Some("veth123".to_string()));
        assert_eq!(allocation.veth_container, Some("eth0".to_string()));
    }
    
    #[tokio::test]
    async fn test_ip_exhaustion() {
        let temp_file = NamedTempFile::new().unwrap();
        let db_path = temp_file.path().to_str().unwrap();
        
        let conn_manager = ConnectionManager::new(db_path).await.unwrap();
        let schema_manager = SchemaManager::new(conn_manager.pool().clone());
        schema_manager.initialize_schema().await.unwrap();
        
        // Create network manager with very small IP range
        let network_manager = NetworkManager::with_ip_range(
            conn_manager.pool().clone(),
            Ipv4Addr::new(172, 16, 0, 10),
            Ipv4Addr::new(172, 16, 0, 11) // Only 2 IPs available
        );
        
        // Allocate first IP
        let config1 = network_manager.allocate_network("container1").await.unwrap();
        assert_eq!(config1.ip_address, "172.16.0.10");
        
        // Allocate second IP
        let config2 = network_manager.allocate_network("container2").await.unwrap();
        assert_eq!(config2.ip_address, "172.16.0.11");
        
        // Third allocation should fail
        let result = network_manager.allocate_network("container3").await;
        assert!(matches!(result, Err(SyncError::NoAvailableIp)));
    }
} 