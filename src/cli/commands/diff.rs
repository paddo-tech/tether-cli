use crate::cli::Output;
use crate::config::Config;
use crate::sync::{GitBackend, MachineState, SyncEngine, SyncState};
use anyhow::Result;
use comfy_table::{presets::UTF8_FULL, Attribute, Cell, Color, ContentArrangement, Table};
use owo_colors::OwoColorize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

fn format_diff_line(symbol: &str, status: &str, pkg: &str) -> String {
    match status {
        "added" => format!("  {} {}", symbol.green(), pkg),
        "removed" => format!("  {} {}", symbol.red(), pkg),
        _ => format!("  {} {}", symbol.yellow(), pkg),
    }
}

pub async fn run(machine: Option<&str>) -> Result<()> {
    let config = match Config::load() {
        Ok(c) => c,
        Err(_) => {
            Output::error("Tether is not initialized. Run 'tether init' first.");
            return Ok(());
        }
    };

    let state = SyncState::load()?;
    let sync_path = SyncEngine::sync_path()?;
    let home = home::home_dir().ok_or_else(|| anyhow::anyhow!("Could not find home directory"))?;

    // Pull latest to ensure we have current remote state
    Output::info("Fetching latest changes...");
    let git = GitBackend::open(&sync_path)?;
    git.pull()?;

    println!();
    println!("{}", "🔍 Tether Diff".bright_cyan().bold());
    println!();

    if let Some(target_machine) = machine {
        // Compare with specific machine
        match MachineState::load_from_repo(&sync_path, target_machine)? {
            Some(other_machine) => {
                // Build current machine state for comparison
                let current_state = build_current_machine_state(&config, &state, &home)?;
                show_machine_diff(&current_state, &other_machine)?;
            }
            None => {
                Output::error(&format!("Machine '{}' not found", target_machine));
                Output::info("Use 'tether machines list' to see available machines");

                // List available machines
                let machines = MachineState::list_all(&sync_path)?;
                if !machines.is_empty() {
                    println!();
                    Output::info("Available machines:");
                    for m in machines {
                        println!("  • {}", m.machine_id);
                    }
                }
            }
        }
    } else {
        // Compare local vs sync repo
        show_dotfile_diff(&config, &state, &sync_path, &home)?;
        show_package_diff(&config, &sync_path).await?;
    }

    Ok(())
}

fn show_dotfile_diff(
    config: &Config,
    state: &SyncState,
    sync_path: &std::path::Path,
    home: &std::path::Path,
) -> Result<()> {
    let dotfiles_dir = sync_path.join("dotfiles");

    let mut diffs: Vec<(String, String, String)> = Vec::new(); // (file, status, details)

    for entry in &config.dotfiles.files {
        let file = entry.path();
        let local_path = home.join(file);
        let filename = file.trim_start_matches('.');

        // Check both encrypted and plain versions
        let remote_path = if config.security.encrypt_dotfiles {
            dotfiles_dir.join(format!("{}.enc", filename))
        } else {
            dotfiles_dir.join(filename)
        };

        let local_exists = local_path.exists();
        let remote_exists = remote_path.exists();

        match (local_exists, remote_exists) {
            (true, true) => {
                // Both exist - check if different
                let local_content = std::fs::read(&local_path)?;
                let local_hash = format!("{:x}", Sha256::digest(&local_content));

                let is_different = state
                    .files
                    .get(file)
                    .map(|f| f.hash != local_hash)
                    .unwrap_or(true);

                if is_different {
                    diffs.push((
                        file.to_string(),
                        "modified".to_string(),
                        "local changes".to_string(),
                    ));
                }
            }
            (true, false) => {
                diffs.push((
                    file.to_string(),
                    "local only".to_string(),
                    "not in sync repo".to_string(),
                ));
            }
            (false, true) => {
                diffs.push((
                    file.to_string(),
                    "remote only".to_string(),
                    "missing locally".to_string(),
                ));
            }
            (false, false) => {
                // Neither exists - skip
            }
        }
    }

    if diffs.is_empty() {
        println!("{}", "📁 Dotfiles: All synced ✓".green());
    } else {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("📁 Dotfiles")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Status")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Details")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

        for (file, status, details) in &diffs {
            let status_color = match status.as_str() {
                "modified" => Color::Yellow,
                "local only" => Color::Green,
                "remote only" => Color::Red,
                _ => Color::White,
            };

            table.add_row(vec![
                Cell::new(file),
                Cell::new(status).fg(status_color),
                Cell::new(details),
            ]);
        }

        println!("{table}");
    }

    println!();
    Ok(())
}

async fn show_package_diff(config: &Config, sync_path: &std::path::Path) -> Result<()> {
    use crate::packages::{BrewManager, NpmManager, PackageManager, PnpmManager};

    let manifests_dir = sync_path.join("manifests");
    let mut has_diff = false;

    // Homebrew diff
    if config.packages.brew.enabled {
        let brew = BrewManager::new();
        if brew.is_available().await {
            let brewfile_path = manifests_dir.join("Brewfile");
            if brewfile_path.exists() {
                let remote_manifest = std::fs::read_to_string(&brewfile_path)?;
                let local_manifest = brew.export_manifest().await?;

                let remote_packages = parse_brewfile(&remote_manifest);
                let local_packages = parse_brewfile(&local_manifest);

                let diff = diff_packages(&remote_packages, &local_packages);
                if !diff.is_empty() {
                    has_diff = true;
                    println!("{}", "📦 Homebrew:".bright_cyan().bold());
                    for (pkg, status) in diff {
                        let symbol = match status.as_str() {
                            "added" => "+",
                            "removed" => "-",
                            _ => "~",
                        };
                        println!("{}", format_diff_line(symbol, &status, &pkg));
                    }
                    println!();
                }
            }
        }
    }

    // npm diff
    if config.packages.npm.enabled {
        let npm = NpmManager::new();
        if npm.is_available().await {
            let npm_path = manifests_dir.join("npm.txt");
            if npm_path.exists() {
                let remote_manifest = std::fs::read_to_string(&npm_path)?;
                let local_manifest = npm.export_manifest().await?;

                let remote_packages: Vec<_> =
                    remote_manifest.lines().filter(|l| !l.is_empty()).collect();
                let local_packages: Vec<_> =
                    local_manifest.lines().filter(|l| !l.is_empty()).collect();

                let diff = diff_package_lists(&remote_packages, &local_packages);
                if !diff.is_empty() {
                    has_diff = true;
                    println!("{}", "📦 npm:".bright_cyan().bold());
                    for (pkg, status) in diff {
                        let symbol = match status.as_str() {
                            "added" => "+",
                            "removed" => "-",
                            _ => "~",
                        };
                        println!("{}", format_diff_line(symbol, &status, &pkg));
                    }
                    println!();
                }
            }
        }
    }

    // pnpm diff
    if config.packages.pnpm.enabled {
        let pnpm = PnpmManager::new();
        if pnpm.is_available().await {
            let pnpm_path = manifests_dir.join("pnpm.txt");
            if pnpm_path.exists() {
                let remote_manifest = std::fs::read_to_string(&pnpm_path)?;
                let local_manifest = pnpm.export_manifest().await?;

                let remote_packages: Vec<_> =
                    remote_manifest.lines().filter(|l| !l.is_empty()).collect();
                let local_packages: Vec<_> =
                    local_manifest.lines().filter(|l| !l.is_empty()).collect();

                let diff = diff_package_lists(&remote_packages, &local_packages);
                if !diff.is_empty() {
                    has_diff = true;
                    println!("{}", "📦 pnpm:".bright_cyan().bold());
                    for (pkg, status) in diff {
                        let symbol = match status.as_str() {
                            "added" => "+",
                            "removed" => "-",
                            _ => "~",
                        };
                        println!("{}", format_diff_line(symbol, &status, &pkg));
                    }
                    println!();
                }
            }
        }
    }

    if !has_diff {
        println!("{}", "📦 Packages: All synced ✓".green());
        println!();
    }

    Ok(())
}

fn parse_brewfile(content: &str) -> HashMap<String, String> {
    let mut packages = HashMap::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Parse lines like: brew "package" or cask "package" or tap "org/repo"
        if let Some(rest) = line
            .strip_prefix("brew ")
            .or_else(|| line.strip_prefix("cask "))
            .or_else(|| line.strip_prefix("tap "))
        {
            let pkg = rest.trim_matches('"').trim_matches('\'');
            let pkg_type = if line.starts_with("cask") {
                "cask"
            } else if line.starts_with("tap") {
                "tap"
            } else {
                "brew"
            };
            packages.insert(pkg.to_string(), pkg_type.to_string());
        }
    }
    packages
}

fn diff_packages(
    remote: &HashMap<String, String>,
    local: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut diff = Vec::new();

    // Packages in local but not in remote (added locally)
    for (pkg, pkg_type) in local {
        if !remote.contains_key(pkg) {
            diff.push((format!("{} ({})", pkg, pkg_type), "added".to_string()));
        }
    }

    // Packages in remote but not in local (removed locally)
    for (pkg, pkg_type) in remote {
        if !local.contains_key(pkg) {
            diff.push((format!("{} ({})", pkg, pkg_type), "removed".to_string()));
        }
    }

    diff.sort_by(|a, b| a.0.cmp(&b.0));
    diff
}

fn diff_package_lists(remote: &[&str], local: &[&str]) -> Vec<(String, String)> {
    let mut diff = Vec::new();

    for pkg in local {
        if !remote.contains(pkg) {
            diff.push((pkg.to_string(), "added".to_string()));
        }
    }

    for pkg in remote {
        if !local.contains(pkg) {
            diff.push((pkg.to_string(), "removed".to_string()));
        }
    }

    diff.sort_by(|a, b| a.0.cmp(&b.0));
    diff
}

fn build_current_machine_state(
    config: &Config,
    state: &SyncState,
    home: &std::path::Path,
) -> Result<MachineState> {
    let mut machine = MachineState::new(&state.machine_id);

    // Collect file hashes
    for entry in &config.dotfiles.files {
        let file = entry.path();
        let path = home.join(file);
        if path.exists() {
            let content = std::fs::read(&path)?;
            let hash = format!("{:x}", sha2::Sha256::digest(&content));
            machine.files.insert(file.to_string(), hash);
        }
    }

    // Collect packages from state
    for (manager, pkg_state) in &state.packages {
        machine
            .packages
            .insert(manager.clone(), vec![pkg_state.hash.clone()]);
    }

    Ok(machine)
}

fn show_machine_diff(current: &MachineState, other: &MachineState) -> Result<()> {
    println!(
        "Comparing {} ({}) vs {} ({})",
        current.machine_id.cyan(),
        current.hostname.dimmed(),
        other.machine_id.cyan(),
        other.hostname.dimmed()
    );
    println!();

    // File differences
    let current_files: HashSet<_> = current.files.keys().collect();
    let other_files: HashSet<_> = other.files.keys().collect();

    let mut file_diffs = Vec::new();

    for file in current_files.difference(&other_files) {
        file_diffs.push(((*file).clone(), "only on this machine".to_string()));
    }
    for file in other_files.difference(&current_files) {
        file_diffs.push(((*file).clone(), "only on other machine".to_string()));
    }
    for file in current_files.intersection(&other_files) {
        if current.files.get(*file) != other.files.get(*file) {
            file_diffs.push(((*file).clone(), "content differs".to_string()));
        }
    }

    if file_diffs.is_empty() {
        println!("{}", "📁 Dotfiles: Identical ✓".green());
    } else {
        let mut table = Table::new();
        table
            .load_preset(UTF8_FULL)
            .set_content_arrangement(ContentArrangement::Dynamic)
            .set_header(vec![
                Cell::new("📁 Dotfiles")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
                Cell::new("Difference")
                    .add_attribute(Attribute::Bold)
                    .fg(Color::Cyan),
            ]);

        for (file, diff) in &file_diffs {
            let color = match diff.as_str() {
                "only on this machine" => Color::Green,
                "only on other machine" => Color::Red,
                _ => Color::Yellow,
            };
            table.add_row(vec![Cell::new(file), Cell::new(diff).fg(color)]);
        }
        println!("{table}");
    }
    println!();

    // Package differences
    let current_pkgs: HashSet<_> = current.packages.keys().collect();
    let other_pkgs: HashSet<_> = other.packages.keys().collect();
    let all_managers: HashSet<_> = current_pkgs.union(&other_pkgs).collect();

    let mut has_pkg_diff = false;

    for manager in all_managers {
        let current_list: HashSet<_> = current
            .packages
            .get(*manager)
            .map(|v| v.iter().collect())
            .unwrap_or_default();
        let other_list: HashSet<_> = other
            .packages
            .get(*manager)
            .map(|v| v.iter().collect())
            .unwrap_or_default();

        let mut diffs = Vec::new();
        for pkg in current_list.difference(&other_list) {
            diffs.push(((*pkg).clone(), "added".to_string()));
        }
        for pkg in other_list.difference(&current_list) {
            diffs.push(((*pkg).clone(), "removed".to_string()));
        }

        if !diffs.is_empty() {
            has_pkg_diff = true;
            println!("{}", format!("📦 {}:", manager).bright_cyan().bold());
            for (pkg, status) in diffs {
                let symbol = if status == "added" { "+" } else { "-" };
                println!("{}", format_diff_line(symbol, &status, &pkg));
            }
            println!();
        }
    }

    if !has_pkg_diff {
        println!("{}", "📦 Packages: Identical ✓".green());
        println!();
    }

    Ok(())
}
