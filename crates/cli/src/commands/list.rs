use std::path::PathBuf;

use toolhub_ingestion::skill_md;

pub async fn run() -> anyhow::Result<()> {
    let home = std::env::var("HOME")?;
    let skill_dir: PathBuf = [&home, ".claude", "skills", "design-md"].iter().collect();
    let meta = skill_md::parse_skill_dir(&skill_dir)?;

    println!("{:<20} {:<10} {}", "id", "type", "description");
    println!("{}", "-".repeat(80));
    println!(
        "{:<20} {:<10} {}",
        meta.id,
        format!("{:?}", meta.r#type).to_lowercase(),
        meta.description.as_deref().unwrap_or("")
    );
    Ok(())
}
