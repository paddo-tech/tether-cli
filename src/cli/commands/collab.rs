use crate::cli::{Output, Progress, Prompt};
use crate::config::{CollabConfig, Config};
use crate::github::GitHubCli;
use crate::sync::git::{get_remote_url, normalize_remote_url};
use crate::sync::GitBackend;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Check if collab feature is enabled, return early with message if not
fn require_collab_feature() -> Result<Config> {
    let config = Config::load()?;
    if !config.features.collab_secrets {
        Output::warning("Collab feature is not enabled");
        Output::info("Enable with: tether config features enable collab_secrets");
        anyhow::bail!("collab_secrets feature is disabled");
    }
    Ok(config)
}

/// Validate collab name is safe for path construction
fn validate_collab_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name.contains("..")
        || name.starts_with('.')
    {
        anyhow::bail!("Invalid collab name: {}", name);
    }
    Ok(())
}

/// Metadata stored in .tether-collab.toml (for reading)
#[derive(Debug, Default, Deserialize)]
struct CollabMetadata {
    #[serde(default)]
    projects: Vec<String>,
    #[serde(default)]
    authorized: Vec<String>,
}

/// Metadata for writing .tether-collab.toml
#[derive(Debug, Serialize)]
struct CollabMetadataWrite<'a> {
    version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_by: Option<&'a str>,
    projects: &'a [String],
    authorized: &'a [String],
}

/// Initialize a new collab for the current project
pub async fn init(project_path: Option<&str>) -> Result<()> {
    let mut config = require_collab_feature()?;

    // Determine project directory
    let project_dir = if let Some(path) = project_path {
        std::path::PathBuf::from(path)
    } else {
        std::env::current_dir()?
    };

    // Get project's git remote
    let remote_url = get_remote_url(&project_dir)?;
    let normalized_url = normalize_remote_url(&remote_url);

    // Parse owner/repo from URL
    let (owner, repo) = GitHubCli::parse_repo_url(&remote_url)
        .ok_or_else(|| anyhow::anyhow!("Could not parse GitHub URL from remote: {}", remote_url))?;

    Output::header("Initialize Collaboration");
    Output::dim(&format!("Project: {}/{}", owner, repo));
    println!();

    // Check if already has a collab
    if config.collab_for_project(&normalized_url).is_some() {
        Output::warning("This project already has a collab configured");
        return Ok(());
    }

    // Ensure GitHub auth
    if !GitHubCli::is_authenticated().await? {
        Output::info("Authenticating with GitHub...");
        GitHubCli::authenticate().await?;
    }

    // Fetch collaborators
    let pb = Progress::spinner("Fetching collaborators...");
    let collaborators = GitHubCli::get_collaborators(&owner, &repo).await?;
    Progress::finish_success(
        &pb,
        &format!("Found {} collaborator(s)", collaborators.len()),
    );

    if collaborators.is_empty() {
        Output::warning("No collaborators with write access found");
        Output::info("Add collaborators to your GitHub repo first");
        return Ok(());
    }

    Output::info("Collaborators with write access:");
    for collab in &collaborators {
        println!("  • {}", collab);
    }
    println!();

    // Create collab sync repo
    let default_repo_name = format!("{}-dotfiles", repo);
    let collab_repo_name = Prompt::input("Collab repo name", Some(&default_repo_name))?;
    validate_collab_name(&collab_repo_name)?;

    let username = GitHubCli::get_username().await?;
    if GitHubCli::repo_exists(&username, &collab_repo_name).await? {
        Output::warning(&format!("{}/{} already exists", username, collab_repo_name));
        if !Prompt::confirm("Use existing repository?", true)? {
            return Ok(());
        }
    } else {
        let pb = Progress::spinner("Creating private collab repository...");
        GitHubCli::create_repo(&collab_repo_name, true).await?;
        Progress::finish_success(&pb, "Repository created");
    }

    let collab_url = format!("git@github.com:{}/{}.git", username, collab_repo_name);

    // Clone collab repo
    let collab_name = collab_repo_name.clone();
    let collab_dir = Config::collab_repo_dir(&collab_name)?;

    let pb = Progress::spinner("Setting up collab repository...");
    std::fs::create_dir_all(collab_dir.parent().unwrap())?;

    if collab_dir.exists() {
        let git = GitBackend::open(&collab_dir)?;
        git.pull()?;
    } else {
        GitBackend::clone(&collab_url, &collab_dir)?;
    }

    // Open git backend once for all subsequent operations
    let git = GitBackend::open(&collab_dir)?;

    // Create recipients directory and add self
    let recipients_dir = collab_dir.join("recipients");
    std::fs::create_dir_all(&recipients_dir)?;

    // Add creator's identity
    if let Ok(identity_pub) = crate::security::get_public_key() {
        let pub_file = recipients_dir.join(format!("{}.pub", username));
        std::fs::write(&pub_file, identity_pub)?;
    }

    // Create projects directory
    std::fs::create_dir_all(collab_dir.join("projects"))?;

    // Create .tether-collab.toml metadata
    let projects = vec![normalized_url.clone()];
    let metadata = CollabMetadataWrite {
        version: 1,
        created_by: Some(&username),
        projects: &projects,
        authorized: &collaborators,
    };
    let metadata_content = format!(
        "# Managed by tether - edit with caution\n{}",
        toml::to_string_pretty(&metadata)?
    );
    std::fs::write(collab_dir.join(".tether-collab.toml"), metadata_content)?;

    // Commit and push
    if git.has_changes()? {
        git.commit("Initialize collab", &username)?;
        git.push()?;
    }
    Progress::finish_success(&pb, "Collab repository ready");

    // Update config
    let teams = config.teams.get_or_insert_with(Default::default);
    teams.collabs.insert(
        collab_name.clone(),
        CollabConfig {
            sync_url: collab_url.clone(),
            projects: vec![normalized_url.clone()],
            members_cache: collaborators,
            last_refresh: Some(chrono::Utc::now()),
            enabled: true,
        },
    );
    config.save()?;

    println!();
    Output::success("Collab initialized!");
    println!();
    Output::info("Share this URL with collaborators:");
    println!("  {}", collab_url);
    println!();
    Output::info("They can join with:");
    println!("  tether collab join {}", collab_url);

    Ok(())
}

/// Join an existing collab
pub async fn join(url: &str) -> Result<()> {
    let mut config = require_collab_feature()?;

    Output::header("Join Collaboration");
    println!();

    // Parse collab name from URL
    let (owner, repo) = GitHubCli::parse_repo_url(url)
        .ok_or_else(|| anyhow::anyhow!("Could not parse GitHub URL: {}", url))?;
    let collab_name = repo.clone();
    validate_collab_name(&collab_name)?;

    // Check if already joined
    if let Some(teams) = &config.teams {
        if teams.collabs.contains_key(&collab_name) {
            Output::warning("Already joined this collab");
            return Ok(());
        }
    }

    // Clone collab repo (temporarily, to read metadata)
    let collab_dir = Config::collab_repo_dir(&collab_name)?;
    std::fs::create_dir_all(collab_dir.parent().unwrap())?;

    let pb = Progress::spinner("Cloning collab repository...");
    GitBackend::clone(url, &collab_dir)?;
    Progress::finish_success(&pb, "Repository cloned");

    // Read metadata to get projects
    let metadata_path = collab_dir.join(".tether-collab.toml");
    let (projects, authorized) = if metadata_path.exists() {
        let content = std::fs::read_to_string(&metadata_path)?;
        match toml::from_str::<CollabMetadata>(&content) {
            Ok(metadata) => (metadata.projects, metadata.authorized),
            Err(e) => {
                Output::warning(&format!("Malformed .tether-collab.toml: {}", e));
                (Vec::new(), Vec::new())
            }
        }
    } else {
        (Vec::new(), Vec::new())
    };

    // Verify user is a collaborator on ALL configured projects
    let username = GitHubCli::get_username().await?;
    if !projects.is_empty() {
        Output::info("Verifying collaborator access...");
        let mut not_collaborator_on: Vec<String> = Vec::new();

        for project_url in &projects {
            // Parse owner/repo from normalized URL (github.com/owner/repo)
            let parts: Vec<&str> = project_url.split('/').collect();
            if parts.len() >= 3 {
                let project_owner = parts[1];
                let project_repo = parts[2];

                match GitHubCli::get_collaborators(project_owner, project_repo).await {
                    Ok(collaborators) => {
                        let is_collab = collaborators
                            .iter()
                            .any(|c| c.eq_ignore_ascii_case(&username));
                        if !is_collab {
                            not_collaborator_on.push(format!("{}/{}", project_owner, project_repo));
                        }
                    }
                    Err(_) => {
                        // Can't verify - might not have access to check collaborators
                        Output::warning(&format!(
                            "Could not verify access to {}/{}",
                            project_owner, project_repo
                        ));
                    }
                }
            }
        }

        if !not_collaborator_on.is_empty() {
            Output::error("You are not a collaborator on these projects:");
            for project in &not_collaborator_on {
                println!("  • {}", project);
            }
            // Clean up cloned repo
            std::fs::remove_dir_all(&collab_dir).ok();
            anyhow::bail!("Must be a GitHub collaborator on all projects to join this collab");
        }
    }

    // Warn about permission drift if collab has authorized list
    if !authorized.is_empty() && !authorized.iter().any(|a| a.eq_ignore_ascii_case(&username)) {
        Output::warning("You're not in the collab's authorized list (permission drift)");
        Output::info("Ask the owner to run 'tether collab refresh' to update the list");
    }

    // Ensure user has identity
    let identity_pub = match crate::security::get_public_key() {
        Ok(key) => key,
        Err(_) => {
            Output::info("Creating age identity...");
            crate::cli::commands::identity::init().await?;
            crate::security::get_public_key()?
        }
    };

    // Add self as recipient
    let recipients_dir = collab_dir.join("recipients");
    std::fs::create_dir_all(&recipients_dir)?;
    let pub_file = recipients_dir.join(format!("{}.pub", username));
    std::fs::write(&pub_file, identity_pub)?;

    // Commit and push
    let git = GitBackend::open(&collab_dir)?;
    if git.has_changes()? {
        git.commit(&format!("Add recipient: {}", username), &username)?;
        git.push()?;
        Output::success(&format!("Added {} as recipient", username));
    }

    // Update config
    let teams = config.teams.get_or_insert_with(Default::default);
    teams.collabs.insert(
        collab_name.clone(),
        CollabConfig {
            sync_url: url.to_string(),
            projects,
            members_cache: vec![owner, username.clone()],
            last_refresh: Some(chrono::Utc::now()),
            enabled: true,
        },
    );
    config.save()?;

    println!();
    Output::success("Joined collab!");
    Output::info("Run 'tether sync' to receive shared secrets");
    Output::warning("Note: Owner must run 'tether collab refresh' to re-encrypt secrets for you");

    Ok(())
}

/// Add a secret file to the collab
pub async fn add(file: &str, project_path: Option<&str>) -> Result<()> {
    let config = require_collab_feature()?;

    // Determine project directory
    let project_dir = if let Some(path) = project_path {
        std::path::PathBuf::from(path)
    } else {
        std::env::current_dir()?
    };

    // Get project's git remote
    let remote_url = get_remote_url(&project_dir)?;
    let normalized_url = normalize_remote_url(&remote_url);

    // Find collab for this project
    let (collab_name, _collab_config) =
        config.collab_for_project(&normalized_url).ok_or_else(|| {
            anyhow::anyhow!(
                "No collab configured for this project. Run 'tether collab init' first."
            )
        })?;

    let collab_dir = Config::collab_repo_dir(&collab_name)?;
    if !collab_dir.exists() {
        return Err(anyhow::anyhow!(
            "Collab repo not found. Try 'tether collab join' again."
        ));
    }

    // Pull latest
    let git = GitBackend::open(&collab_dir)?;
    git.pull()?;

    // Load recipients
    let recipients_dir = collab_dir.join("recipients");
    let recipients = crate::security::load_recipients(&recipients_dir)?;
    if recipients.is_empty() {
        return Err(anyhow::anyhow!(
            "No recipients found in collab. Add recipients first."
        ));
    }

    // Read file to encrypt
    let file_path = project_dir.join(file);
    if !file_path.exists() {
        return Err(anyhow::anyhow!("File not found: {}", file_path.display()));
    }

    // Security: prevent path traversal attacks
    let canonical_project = project_dir.canonicalize()?;
    let canonical_file = file_path.canonicalize()?;
    if !canonical_file.starts_with(&canonical_project) {
        return Err(anyhow::anyhow!(
            "Path traversal not allowed: file must be within project directory"
        ));
    }

    // Use sanitized relative path for destination
    let relative_path = canonical_file
        .strip_prefix(&canonical_project)
        .map_err(|_| anyhow::anyhow!("Failed to compute relative path"))?;

    let content = std::fs::read(&file_path)?;

    // Encrypt to all recipients
    let encrypted = crate::security::encrypt_to_recipients(&content, &recipients)?;

    // Write to collab repo (using sanitized relative path)
    let dest_dir = collab_dir.join("projects").join(&normalized_url);
    std::fs::create_dir_all(&dest_dir)?;
    let dest_file = dest_dir.join(format!("{}.age", relative_path.display()));
    std::fs::write(&dest_file, encrypted)?;

    // Commit and push
    let relative_path_str = relative_path.display().to_string();
    if git.has_changes()? {
        let username = GitHubCli::get_username()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        git.commit(
            &format!("Add secret: {}/{}", normalized_url, relative_path_str),
            &username,
        )?;
        git.push()?;
    }

    Output::success(&format!("Added {} to collab", relative_path_str));
    Output::info(&format!("Encrypted to {} recipient(s)", recipients.len()));

    Ok(())
}

/// Refresh collaborators from GitHub and re-encrypt secrets
pub async fn refresh(project_path: Option<&str>) -> Result<()> {
    let mut config = require_collab_feature()?;

    // Determine project directory
    let project_dir = if let Some(path) = project_path {
        std::path::PathBuf::from(path)
    } else {
        std::env::current_dir()?
    };

    // Get project's git remote
    let remote_url = get_remote_url(&project_dir)?;
    let normalized_url = normalize_remote_url(&remote_url);
    let (collab_name, collab_config) = config
        .collab_for_project(&normalized_url)
        .ok_or_else(|| anyhow::anyhow!("No collab configured for this project"))?;
    let collab_name = collab_name.clone();
    let all_projects = collab_config.projects.clone();

    let collab_dir = Config::collab_repo_dir(&collab_name)?;
    if !collab_dir.exists() {
        return Err(anyhow::anyhow!("Collab repo not found"));
    }

    // Parse owner/repo from project URL
    let (owner, repo) = GitHubCli::parse_repo_url(&remote_url)
        .ok_or_else(|| anyhow::anyhow!("Could not parse GitHub URL"))?;

    Output::header("Refresh Collaborators");
    println!();

    // Fetch current collaborators for ALL projects
    let pb = Progress::spinner("Fetching collaborators from GitHub...");
    let mut all_collaborators: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Start with current project
    let collaborators = GitHubCli::get_collaborators(&owner, &repo).await?;
    for c in &collaborators {
        all_collaborators.insert(c.to_lowercase());
    }

    // Check all other projects in this collab
    for project_url in &all_projects {
        let parts: Vec<&str> = project_url.split('/').collect();
        if parts.len() >= 3 {
            let project_owner = parts[1];
            let project_repo = parts[2];
            if let Ok(project_collabs) =
                GitHubCli::get_collaborators(project_owner, project_repo).await
            {
                for c in &project_collabs {
                    all_collaborators.insert(c.to_lowercase());
                }
            }
        }
    }

    let collaborators: Vec<String> = all_collaborators.into_iter().collect();
    Progress::finish_success(
        &pb,
        &format!(
            "Found {} collaborator(s) across {} project(s)",
            collaborators.len(),
            all_projects.len()
        ),
    );

    Output::info("Current collaborators:");
    for collab in &collaborators {
        println!("  • {}", collab);
    }
    println!();

    // Check for permission drift - recipients who aren't collaborators
    let recipients_dir = collab_dir.join("recipients");
    if recipients_dir.exists() {
        let mut drifted: Vec<String> = Vec::new();
        for entry in std::fs::read_dir(&recipients_dir)? {
            let entry = entry?;
            let filename = entry.file_name().to_string_lossy().to_string();
            if filename.ends_with(".pub") {
                let recipient_name = filename.trim_end_matches(".pub");
                if !collaborators
                    .iter()
                    .any(|c| c.eq_ignore_ascii_case(recipient_name))
                {
                    drifted.push(recipient_name.to_string());
                }
            }
        }

        if !drifted.is_empty() {
            println!();
            Output::warning(
                "Permission drift detected! These recipients are no longer collaborators:",
            );
            for name in &drifted {
                println!("  • {}", name);
            }
            Output::info("They still have access to secrets until you remove their .pub files from the collab repo");
            println!();
        }
    }

    // Update members cache
    if let Some(teams) = &mut config.teams {
        if let Some(collab_config) = teams.collabs.get_mut(&collab_name) {
            collab_config.members_cache = collaborators.clone();
            collab_config.last_refresh = Some(chrono::Utc::now());
        }
    }
    config.save()?;

    // Pull latest collab repo
    let git = GitBackend::open(&collab_dir)?;
    git.pull()?;

    // Update metadata with current authorized list
    let metadata_path = collab_dir.join(".tether-collab.toml");
    if metadata_path.exists() {
        if let Some(teams) = &config.teams {
            if let Some(collab) = teams.collabs.get(&collab_name) {
                let metadata = CollabMetadataWrite {
                    version: 1,
                    created_by: None,
                    projects: &collab.projects,
                    authorized: &collaborators,
                };
                let metadata_content = format!(
                    "# Managed by tether - edit with caution\n{}",
                    toml::to_string_pretty(&metadata)?
                );
                std::fs::write(&metadata_path, metadata_content)?;
            }
        }
    }

    // Re-encrypt all secrets with current recipients
    let recipients = crate::security::load_recipients(&recipients_dir)?;

    if recipients.is_empty() {
        Output::warning("No recipients found - secrets won't be re-encrypted");
        return Ok(());
    }

    let projects_dir = collab_dir.join("projects");
    if !projects_dir.exists() {
        Output::info("No secrets to re-encrypt");
        return Ok(());
    }

    // Load user's identity for decryption
    let identity = crate::security::load_identity(None)?;

    let pb = Progress::spinner("Re-encrypting secrets...");
    let mut count = 0;

    for entry in walkdir::WalkDir::new(&projects_dir) {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        if !entry.file_type().is_file() {
            continue;
        }

        let path = entry.path();
        if !path.to_string_lossy().ends_with(".age") {
            continue;
        }

        // Decrypt with identity
        let encrypted = std::fs::read(path)?;
        match crate::security::decrypt_with_identity(&encrypted, &identity) {
            Ok(decrypted) => {
                // Re-encrypt to current recipients
                let re_encrypted = crate::security::encrypt_to_recipients(&decrypted, &recipients)?;
                std::fs::write(path, re_encrypted)?;
                count += 1;
            }
            Err(e) => {
                Output::warning(&format!("Could not decrypt {}: {}", path.display(), e));
            }
        }
    }

    Progress::finish_success(&pb, &format!("Re-encrypted {} secret(s)", count));

    // Commit and push
    if git.has_changes()? {
        let username = GitHubCli::get_username()
            .await
            .unwrap_or_else(|_| "unknown".to_string());
        git.commit("Re-encrypt secrets for updated recipients", &username)?;
        git.push()?;
        Output::success("Pushed updated secrets");
    }

    Ok(())
}

/// List all collabs
pub async fn list() -> Result<()> {
    let config = require_collab_feature()?;

    let collabs = match &config.teams {
        Some(t) if !t.collabs.is_empty() => &t.collabs,
        _ => {
            Output::info("No collabs configured");
            Output::info("Run 'tether collab init' in a project directory to create one");
            return Ok(());
        }
    };

    Output::section("Collaborations");
    println!();

    for (name, collab) in collabs {
        let status = if collab.enabled {
            "enabled"
        } else {
            "disabled"
        };
        println!("{} ({})", name, status);
        println!("  URL: {}", collab.sync_url);
        println!("  Projects: {}", collab.projects.join(", "));
        println!("  Members: {}", collab.members_cache.join(", "));
        if let Some(refresh) = &collab.last_refresh {
            println!("  Last refresh: {}", refresh.format("%Y-%m-%d %H:%M UTC"));
        }
        println!();
    }

    Ok(())
}

/// Add another project to an existing collab
pub async fn add_project(project_path: &str) -> Result<()> {
    let mut config = require_collab_feature()?;

    let project_dir = std::path::PathBuf::from(project_path);
    if !project_dir.exists() {
        return Err(anyhow::anyhow!(
            "Project directory not found: {}",
            project_path
        ));
    }

    // Get project's git remote
    let remote_url = get_remote_url(&project_dir)?;
    let normalized_url = normalize_remote_url(&remote_url);

    // Check if already in a collab
    if config.collab_for_project(&normalized_url).is_some() {
        Output::warning("This project already has a collab");
        return Ok(());
    }

    // List available collabs
    let collabs: Vec<String> = config
        .teams
        .as_ref()
        .map(|t| t.collabs.keys().cloned().collect())
        .unwrap_or_default();

    if collabs.is_empty() {
        Output::error("No collabs available. Create one with 'tether collab init' first.");
        return Ok(());
    }

    // Select collab to add to
    let options: Vec<&str> = collabs.iter().map(|s| s.as_str()).collect();
    let selection = Prompt::select("Add project to which collab?", options, 0)?;
    let collab_name = &collabs[selection];

    // Update config
    if let Some(teams) = &mut config.teams {
        if let Some(collab) = teams.collabs.get_mut(collab_name) {
            if !collab.projects.contains(&normalized_url) {
                collab.projects.push(normalized_url.clone());
            }
        }
    }
    config.save()?;

    // Update collab repo metadata
    let collab_dir = Config::collab_repo_dir(collab_name)?;
    if collab_dir.exists() {
        let metadata_path = collab_dir.join(".tether-collab.toml");
        if metadata_path.exists() {
            // Update projects in metadata
            if let Some(teams) = &config.teams {
                if let Some(collab) = teams.collabs.get(collab_name) {
                    let metadata = CollabMetadataWrite {
                        version: 1,
                        created_by: None,
                        projects: &collab.projects,
                        authorized: &collab.members_cache,
                    };
                    let metadata_content = format!(
                        "# Managed by tether - edit with caution\n{}",
                        toml::to_string_pretty(&metadata)?
                    );
                    std::fs::write(&metadata_path, metadata_content)?;

                    let git = GitBackend::open(&collab_dir)?;
                    if git.has_changes()? {
                        let username = GitHubCli::get_username()
                            .await
                            .unwrap_or_else(|_| "unknown".to_string());
                        git.commit(&format!("Add project: {}", normalized_url), &username)?;
                        git.push()?;
                    }
                }
            }
        }
    }

    Output::success(&format!(
        "Added {} to collab '{}'",
        normalized_url, collab_name
    ));
    Ok(())
}

/// Remove a collab
pub async fn remove(collab_name: Option<&str>) -> Result<()> {
    let mut config = require_collab_feature()?;

    let name = if let Some(n) = collab_name {
        n.to_string()
    } else {
        // List available collabs
        let collabs: Vec<String> = config
            .teams
            .as_ref()
            .map(|t| t.collabs.keys().cloned().collect())
            .unwrap_or_default();

        if collabs.is_empty() {
            Output::info("No collabs to remove");
            return Ok(());
        }

        let options: Vec<&str> = collabs.iter().map(|s| s.as_str()).collect();
        let selection = Prompt::select("Remove which collab?", options, 0)?;
        collabs[selection].clone()
    };
    validate_collab_name(&name)?;

    if !Prompt::confirm(&format!("Remove collab '{}'?", name), false)? {
        return Ok(());
    }

    // Remove from config
    if let Some(teams) = &mut config.teams {
        teams.collabs.remove(&name);
    }
    config.save()?;

    // Remove local directory
    let collab_dir = Config::collab_dir(&name)?;
    if collab_dir.exists() {
        std::fs::remove_dir_all(&collab_dir)?;
    }

    Output::success(&format!("Removed collab '{}'", name));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_collab_name_valid() {
        assert!(validate_collab_name("my-project").is_ok());
        assert!(validate_collab_name("project_dotfiles").is_ok());
        assert!(validate_collab_name("MyProject123").is_ok());
    }

    #[test]
    fn test_validate_collab_name_empty() {
        assert!(validate_collab_name("").is_err());
    }

    #[test]
    fn test_validate_collab_name_path_traversal() {
        assert!(validate_collab_name("..").is_err());
        assert!(validate_collab_name("foo/bar").is_err());
        assert!(validate_collab_name("foo\\bar").is_err());
        assert!(validate_collab_name("../etc").is_err());
        assert!(validate_collab_name("foo/..").is_err());
    }

    #[test]
    fn test_validate_collab_name_hidden() {
        assert!(validate_collab_name(".hidden").is_err());
        assert!(validate_collab_name(".git").is_err());
    }
}
