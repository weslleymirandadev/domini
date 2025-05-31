use std::{
    fs::{self, File},
    io::Read,
};

use tracing::info;
use uuid::Uuid;

const ID_FILE: &str = ".unique_id";

pub async fn get_or_create_id() -> Result<String, String> {
    let home_dir = dirs::home_dir().ok_or_else(|| "Could not get home directory".to_string())?;
    let id_path = home_dir.join(ID_FILE);

    if id_path.exists() {
        let mut file = File::open(&id_path)
            .map_err(|e| format!("Error opening ID file {}: {}", id_path.display(), e))?;
        let mut id = String::new();
        file.read_to_string(&mut id)
            .map_err(|e| format!("Error reading ID file {}: {}", id_path.display(), e))?;
        info!("UUID loaded: {} from {}", id.trim(), id_path.display());
        Ok(id.trim().to_string())
    } else {
        let id = Uuid::new_v4().to_string();
        fs::write(&id_path, &id)
            .map_err(|e| format!("Error writing ID file {}: {}", id_path.display(), e))?;
        info!("New UUID created: {} in {}", id, id_path.display());
        Ok(id)
    }
}
