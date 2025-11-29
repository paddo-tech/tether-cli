use crate::cli::Output;
use crate::config::Config;
use anyhow::Result;
use std::path::PathBuf;

fn ignore_file_path() -> Result<PathBuf> {
    Ok(Config::config_dir()?.join("ignore"))
}

pub async fn add(pattern: &str) -> Result<()> {
    let path = ignore_file_path()?;

    // Read existing patterns
    let mut patterns = if path.exists() {
        std::fs::read_to_string(&path)?
            .lines()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    // Check if pattern already exists
    if patterns.iter().any(|p| p == pattern) {
        Output::warning(&format!("Pattern '{}' already exists", pattern));
        return Ok(());
    }

    // Add pattern
    patterns.push(pattern.to_string());

    // Write back
    std::fs::write(&path, patterns.join("\n") + "\n")?;

    Output::success(&format!("Added ignore pattern: {}", pattern));
    Ok(())
}

pub async fn list() -> Result<()> {
    let path = ignore_file_path()?;

    if !path.exists() {
        Output::info("No ignore patterns configured");
        Output::info("Add patterns with: tether ignore add <pattern>");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let patterns: Vec<_> = content.lines().filter(|l| !l.is_empty()).collect();

    if patterns.is_empty() {
        Output::info("No ignore patterns configured");
        return Ok(());
    }

    println!();
    println!("Ignore patterns:");
    for pattern in patterns {
        println!("  • {}", pattern);
    }
    println!();

    Output::info(&format!("File: {}", path.display()));
    Ok(())
}

pub async fn remove(pattern: &str) -> Result<()> {
    let path = ignore_file_path()?;

    if !path.exists() {
        Output::error("No ignore patterns configured");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path)?;
    let patterns: Vec<_> = content
        .lines()
        .filter(|l| !l.is_empty() && *l != pattern)
        .collect();

    if patterns.len() == content.lines().filter(|l| !l.is_empty()).count() {
        Output::error(&format!("Pattern '{}' not found", pattern));
        return Ok(());
    }

    std::fs::write(&path, patterns.join("\n") + "\n")?;

    Output::success(&format!("Removed ignore pattern: {}", pattern));
    Ok(())
}
