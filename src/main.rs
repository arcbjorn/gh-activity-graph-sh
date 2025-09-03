use std::io::{stdout, Write};
use std::time::Duration;

use anyhow::{Result, Context};
use chrono::{Utc, Datelike, NaiveDate, TimeZone};
use clap::Parser;
use colored::*;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind, KeyModifiers},
    terminal,
};
use serde::{Deserialize, Serialize};

// Constants
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const WEEKS_IN_YEAR: usize = 52;
const DAY_LABELS: &[&str] = &["   Mon", "      ", "   Wed", "      ", "   Fri", "      "];
const MONTH_SPACING: usize = 10;

#[derive(Parser)]
#[command(name = "github-stats")]
#[command(about = "Display GitHub contribution statistics")]
struct Cli {
    /// GitHub username to analyze
    username: Option<String>,
    
    /// GitHub personal access token
    #[arg(short, long, env)]
    token: Option<String>,
    
    /// Output format (text, json)
    #[arg(short, long, default_value = "text")]
    format: String,
}

#[derive(Debug, Deserialize)]
struct User {
    login: String,
}


#[derive(Debug, Deserialize)]
struct GraphQLResponse {
    data: GraphQLData,
}

#[derive(Debug, Deserialize)]
struct GraphQLData {
    user: GraphQLUser,
}

#[derive(Debug, Deserialize)]
struct GraphQLUser {
    #[serde(rename = "contributionsCollection")]
    contributions_collection: ContributionsCollection,
    repositories: RepositoryConnection,
}

#[derive(Debug, Deserialize)]
struct ContributionsCollection {
    #[serde(rename = "contributionCalendar")]
    contribution_calendar: ContributionCalendar,
}

#[derive(Debug, Deserialize)]
struct ContributionCalendar {
    #[serde(rename = "totalContributions")]
    total_contributions: u32,
    weeks: Vec<GraphQLWeek>,
}

#[derive(Debug, Deserialize)]
struct GraphQLWeek {
    #[serde(rename = "contributionDays")]
    contribution_days: Vec<GraphQLDay>,
}

#[derive(Debug, Deserialize)]
struct GraphQLDay {
    date: String,
    #[serde(rename = "contributionCount")]
    contribution_count: u32,
    #[serde(rename = "contributionLevel")]
    contribution_level: String,
}

#[derive(Debug, Serialize)]
struct Stats {
    username: String,
    contribution_graph: ContributionGraph,
    recent_repos: Vec<RepositoryWithCommits>,
}

#[derive(Debug, Deserialize)]
struct RepositoryConnection {
    nodes: Vec<Repository>,
}

#[derive(Debug, Deserialize, Serialize)]
struct Repository {
    name: String,
    #[serde(rename = "pushedAt")]
    pushed_at: String,
    owner: RepositoryOwner,
}

#[derive(Debug, Deserialize, Serialize)]
struct RepositoryOwner {
    login: String,
}

#[derive(Debug, Serialize)]
struct RepositoryWithCommits {
    name: String,
    full_name: String,
    pushed_at: String,
    today_commits: u32,
    week_commits: u32,
    month_commits: u32,
}

#[derive(Debug, Serialize)]
struct ContributionGraph {
    weeks: Vec<Week>,
    total_contributions: u32,
}

#[derive(Debug, Serialize)]
struct Week {
    days: Vec<Day>,
}

#[derive(Debug, Serialize, Clone)]
struct Day {
    date: String,
    count: u32,
    level: u8, // 0-4 for different intensity levels
}

struct GitHubClient {
    client: reqwest::Client,
    username: String,
}

impl GitHubClient {
    fn new(username: String, token: Option<String>) -> Result<Self> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::USER_AGENT,
            "GitHub-Stats-CLI-Rust".parse()?,
        );
        headers.insert(
            reqwest::header::ACCEPT,
            "application/vnd.github.v3+json".parse()?,
        );

        // Try to get token from gh CLI if not provided
        let auth_token = if let Some(token) = token {
            Some(token)
        } else {
            Self::get_gh_token().ok()
        };

        if let Some(token) = auth_token {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", token).parse()?,
            );
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()?;

        Ok(Self { client, username })
    }
    
    fn get_gh_token() -> Result<String> {
        let output = std::process::Command::new("gh")
            .args(&["auth", "token"])
            .output()
            .context("Failed to run 'gh auth token' command")?;
        
        if !output.status.success() {
            anyhow::bail!("gh CLI not authenticated. Run 'gh auth login' first.");
        }
        
        let token = String::from_utf8(output.stdout)?
            .trim()
            .to_string();
            
        if token.is_empty() {
            anyhow::bail!("No token returned from gh CLI");
        }
        
        Ok(token)
    }

    async fn get_user(&self) -> Result<User> {
        let url = format!("https://api.github.com/users/{}", self.username);
        let response = self.client.get(&url).send().await?;
        
        if response.status() == 404 {
            anyhow::bail!("User '{}' not found", self.username);
        }
        
        let user: User = response.json().await?;
        Ok(user)
    }

    
    async fn get_data_from_graphql(&self) -> Result<(ContributionGraph, Vec<RepositoryWithCommits>)> {
        let query = r#" 
        query($username: String!) {
            user(login: $username) {
                contributionsCollection {
                    contributionCalendar {
                        totalContributions
                        weeks {
                            contributionDays {
                                date
                                contributionCount
                                contributionLevel
                            }
                        }
                    }
                }
                repositories(
                    first: 5
                    orderBy: {field: PUSHED_AT, direction: DESC}
                    ownerAffiliations: [OWNER, COLLABORATOR]
                ) {
                    nodes {
                        name
                        pushedAt
                        owner {
                            login
                        }
                    }
                }
            }
        }
        "#;
        
        let variables = serde_json::json!({
            "username": self.username
        });
        
        let request_body = serde_json::json!({
            "query": query,
            "variables": variables
        });
        
        let response = self.client
            .post("https://api.github.com/graphql")
            .json(&request_body)
            .send()
            .await?;
            
        if !response.status().is_success() {
            anyhow::bail!("GraphQL request failed: {}", response.status());
        }
        
        let graphql_response: GraphQLResponse = response.json().await?;
        let user_data = graphql_response.data.user;
        let calendar = user_data.contributions_collection.contribution_calendar;
        
        // Convert GraphQL data to our format
        let mut weeks = Vec::new();
        
        for graphql_week in calendar.weeks {
            let mut week = Week { days: Vec::new() };
            
            for graphql_day in graphql_week.contribution_days {
                let level = match graphql_day.contribution_level.as_str() {
                    "NONE" => 0,
                    "FIRST_QUARTILE" => 1,
                    "SECOND_QUARTILE" => 2,
                    "THIRD_QUARTILE" => 3,
                    "FOURTH_QUARTILE" => 4,
                    _ => 0,
                };
                
                week.days.push(Day {
                    date: graphql_day.date,
                    count: graphql_day.contribution_count,
                    level,
                });
            }
            
            weeks.push(week);
        }
        
        let contribution_graph = ContributionGraph {
            weeks,
            total_contributions: calendar.total_contributions,
        };

        // Get commit counts for each repository (single API call per repo)
        let mut repos_with_commits = Vec::new();
        for repo in user_data.repositories.nodes {
            let full_name = format!("{}/{}", repo.owner.login, repo.name);
            let (today_commits, week_commits, month_commits) = self.get_all_commit_counts(&full_name).await.unwrap_or((0, 0, 0));
            repos_with_commits.push(RepositoryWithCommits {
                name: repo.name,
                full_name: full_name.clone(),
                pushed_at: repo.pushed_at,
                today_commits,
                week_commits,
                month_commits,
            });
        }

        Ok((contribution_graph, repos_with_commits))
    }

    /// Get commit counts for today, this week, and this month for a repository
    /// Uses the same time period calculations as the main stats to ensure consistency
    async fn get_all_commit_counts(&self, full_repo_name: &str) -> Result<(u32, u32, u32)> {
        // Use local time zone (same as main stats) to ensure consistency
        let today = chrono::Local::now().date_naive();
        let today_start = chrono::Local.from_local_datetime(&today.and_hms_opt(0, 0, 0).unwrap()).unwrap().with_timezone(&chrono::Utc);
        let today_end = chrono::Local.from_local_datetime(&today.and_hms_opt(23, 59, 59).unwrap()).unwrap().with_timezone(&chrono::Utc);

        // This week: Monday of current week to now (same calculation as main stats)
        let week_start_date = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
        let week_start = chrono::Local.from_local_datetime(&week_start_date.and_hms_opt(0, 0, 0).unwrap()).unwrap().with_timezone(&chrono::Utc);

        // This month: 1st day of current month to now (same calculation as main stats)
        let month_start_date = today.with_day(1).unwrap();
        let month_start = chrono::Local.from_local_datetime(&month_start_date.and_hms_opt(0, 0, 0).unwrap()).unwrap().with_timezone(&chrono::Utc);

        // Fetch all commits for the month period in a single API call for efficiency
        let month_start_str = month_start.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let today_end_str = today_end.format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let commits = self.get_commits_with_dates(full_repo_name, &month_start_str, &today_end_str).await?;

        // Count commits by filtering in memory (more efficient than separate API calls)
        let mut today_count = 0;
        let mut week_count = 0;
        let month_count = commits.len() as u32;

        for commit in &commits {
            if let Some(commit_date) = commit.get("commit")
                .and_then(|c| c.get("author"))
                .and_then(|a| a.get("date"))
                .and_then(|d| d.as_str())
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            {
                let commit_date = commit_date.with_timezone(&chrono::Utc);

                if commit_date >= today_start && commit_date <= today_end {
                    today_count += 1;
                }
                if commit_date >= week_start && commit_date <= today_end {
                    week_count += 1;
                }
            }
        }

        Ok((today_count, week_count, month_count))
    }

    /// Fetch commits from GitHub API with pagination, filtered by author
    async fn get_commits_with_dates(&self, full_repo_name: &str, since: &str, until: &str) -> Result<Vec<serde_json::Value>> {
        let mut all_commits = Vec::new();
        let mut page = 1;
        let per_page = 100;

        loop {
            let url = format!(
                "https://api.github.com/repos/{}/commits?since={}&until={}&page={}&per_page={}",
                full_repo_name, since, until, page, per_page
            );

            match self.client.get(&url).send().await {
                Ok(response) => {
                    if response.status().is_success() {
                        let commits: Vec<serde_json::Value> = response.json().await?;
                        if commits.is_empty() {
                            break;
                        }

                        // Filter commits by GitHub author (not Git author) for accuracy
                        let user_commits: Vec<serde_json::Value> = commits.into_iter().filter(|commit| {
                            if let Some(author) = commit.get("author") {
                                if let Some(login) = author.get("login") {
                                    return login.as_str() == Some(&self.username);
                                }
                            }
                            false
                        }).collect();

                        all_commits.extend(user_commits);
                        page += 1;

                        // Limit pagination to avoid rate limits
                        if page > 5 {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                Err(_) => break,
            }
        }

        Ok(all_commits)
    }
    
    async fn generate_data(&self) -> Result<(ContributionGraph, Vec<RepositoryWithCommits>)> {
        match self.get_data_from_graphql().await {
            Ok((graph, repos)) => Ok((graph, repos)),
            Err(_) => {
                Ok((
                    ContributionGraph {
                        weeks: Vec::new(),
                        total_contributions: 0,
                    },
                    Vec::new()
                ))
            }
        }
    }


    async fn get_stats(&self) -> Result<Stats> {
        let user = self.get_user().await?;
        let (contribution_graph, recent_repos) = self.generate_data().await?;

        Ok(Stats {
            username: user.login,
            contribution_graph,
            recent_repos,
        })
    }
}

async fn show_loading_animation() {
    let mut frame_idx = 0;
    let placeholder = "⬜".bright_black();
    
    // Print the loading graph once - same dimensions as contribution graph
    println!();
    println!("       Sep          Oct          Nov          Dec          Jan          Feb          Mar          Apr          May          Jun          Jul          Aug");
    
    // Print graph rows
    for (i, day_label) in std::iter::once("       ").chain(DAY_LABELS.iter().copied()).enumerate() {
        if i == 0 {
            print!("{}", day_label);
        } else {
            print!("{} ", day_label);
        }
        
        for _ in 0..WEEKS_IN_YEAR {
            print!("{} ", placeholder);
        }
        println!();
    }
    
    println!();
    print!("Loading {} contributions...", SPINNER_FRAMES[0].bright_blue());
    stdout().flush().unwrap();
    
    // Animate only the spinner
    loop {
        print!("\rLoading {} contributions...", SPINNER_FRAMES[frame_idx].bright_blue());
        stdout().flush().unwrap();
        
        frame_idx = (frame_idx + 1) % SPINNER_FRAMES.len();
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
}

fn display_contribution_graph(stats: &Stats) {
    println!();
    
    // Display month labels
    print!("       ");
    let months = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let total_weeks = stats.contribution_graph.weeks.len();
    
    if total_weeks > 0 {
        let current_month = Utc::now().month0() as usize;
        let start_month = (current_month + 1) % 12;
        
        let spacing = MONTH_SPACING;
        
        for i in 0..12 {
            if i > 0 {
                for _ in 0..spacing {
                    print!(" ");
                }
            }
            
            let month_idx = (start_month + i) % 12;
            print!("{}", months[month_idx]);
        }
    }
    println!();
    
    // Display day labels and contribution graph
    let day_labels = ["", "Mon", "", "Wed", "", "Fri", ""];
    
    for day_of_week in 0..7 {
        print!("{:>6}", day_labels[day_of_week]);
        
        for week in &stats.contribution_graph.weeks {
            if let Some(day) = week.days.get(day_of_week) {
                let symbol = match day.level {
                    0 => "⬛".bright_black(),
                    1 => "🟩".bright_green(), 
                    2 => "🟨".bright_yellow(),
                    3 => "🟧".bright_yellow(), 
                    4 => "🟥".bright_red(),
                    _ => "⬛".bright_black(),
                };
                print!(" {}", symbol);
            } else {
                print!(" ⬛");
            }
        }
        println!();
    }
    
    println!();
    
    // Calculate additional stats
    let today = chrono::Local::now().date_naive();
    let this_week_start = today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64);
    let last_week_start = this_week_start - chrono::Duration::days(7);
    let last_week_end = this_week_start - chrono::Duration::days(1);
    let this_month_start = today.with_day(1).unwrap();
    let this_year_start = today.with_ordinal(1).unwrap();
    
    let mut today_contributions = 0;
    let mut this_week_contributions = 0;
    let mut last_week_contributions = 0;
    let mut this_month_contributions = 0;
    let mut this_year_contributions = 0;
    
    for week in &stats.contribution_graph.weeks {
        for day in &week.days {
            if let Ok(day_date) = NaiveDate::parse_from_str(&day.date, "%Y-%m-%d") {
                if day_date == today {
                    today_contributions = day.count;
                }
                if day_date >= this_week_start {
                    this_week_contributions += day.count;
                }
                if day_date >= last_week_start && day_date <= last_week_end {
                    last_week_contributions += day.count;
                }
                if day_date >= this_month_start {
                    this_month_contributions += day.count;
                }
                if day_date >= this_year_start {
                    this_year_contributions += day.count;
                }
            }
        }
    }
    
    // Week comparison
    let week_diff = this_week_contributions as i32 - last_week_contributions as i32;
    let comparison = if week_diff > 0 {
        format!(" ({} more than last week)", week_diff.to_string().bright_green())
    } else if week_diff < 0 {
        format!(" ({} less than last week)", (-week_diff).to_string().bright_red())
    } else {
        " (same as last week)".to_string()
    };
    
    // Single line with all stats and comparison
    println!("Today: {} | This week: {}{} | This month: {} | This year: {}", 
        today_contributions.to_string().bright_green(),
        this_week_contributions.to_string().bright_green(),
        comparison,
        this_month_contributions.to_string().bright_green(),
        this_year_contributions.to_string().bright_green()
    );
    
    // Legend
    println!();
    print!("Less ");
    print!("{} ", "⬛".bright_black());
    print!("{} ", "🟩".bright_green());
    print!("{} ", "🟨".bright_yellow());
    print!("{} ", "🟧".bright_yellow());
    print!("{} ", "🟥".bright_red());
    println!("More");

    // Display latest updated repositories with commit counts
    if !stats.recent_repos.is_empty() {
        println!();
        println!("{}", "Latest Updated Repositories:".bright_cyan().bold());
        println!();

        // Column headers with color coding
        println!("{:<4} {:<35} {:<8} {:<10} {:<12} {:<15}",
            "No.".bright_white().bold(),
            "Repository".bright_white().bold(),
            "Today".bright_green().bold(),
            "This Week".bright_cyan().bold(),
            "This Month".bright_yellow().bold(),
            "Last Updated".bright_white().bold()
        );

        // Separator line
        println!("{}", "─".repeat(85).bright_black());

        for (i, repo) in stats.recent_repos.iter().take(5).enumerate() {
            // Format the pushed_at time
            let pushed_display = if let Ok(pushed_time) = chrono::DateTime::parse_from_rfc3339(&repo.pushed_at) {
                let now = chrono::Utc::now();
                let duration = now.signed_duration_since(pushed_time);

                if duration.num_days() > 0 {
                    format!("{} days ago", duration.num_days())
                } else if duration.num_hours() > 0 {
                    format!("{} hours ago", duration.num_hours())
                } else if duration.num_minutes() > 0 {
                    format!("{} minutes ago", duration.num_minutes())
                } else {
                    "just now".to_string()
                }
            } else {
                "unknown".to_string()
            };

            println!("{:<4} {:<35} {:<8} {:<10} {:<12} {:<15}",
                format!("{}.", i + 1).bright_white(),
                repo.full_name.bright_blue().bold(),
                repo.today_commits.to_string().bright_green(),
                repo.week_commits.to_string().bright_cyan(),
                repo.month_commits.to_string().bright_yellow(),
                pushed_display.bright_black()
            );
        }
    }
}



#[tokio::main] 
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Get username from args or from authenticated user
    let username = if let Some(username) = cli.username {
        username
    } else {
        // Try to get current authenticated user
        let output = std::process::Command::new("gh")
            .args(&["api", "user", "--jq", ".login"])
            .output();
        
        match output {
            Ok(output) if output.status.success() => {
                String::from_utf8(output.stdout)?.trim().to_string()
            }
            _ => {
                anyhow::bail!("No username provided and couldn't detect authenticated GitHub user. Try: github-stats <username>");
            }
        }
    };

    let client = GitHubClient::new(username, cli.token)
        .context("Failed to create GitHub client")?;

    // Start loading animation in background
    let loading_handle = tokio::spawn(show_loading_animation());
    
    // Fetch stats
    let stats_result = client.get_stats().await;
    
    // Stop loading animation and clear screen
    loading_handle.abort();
    print!("\x1b[2J\x1b[1;1H"); // Clear entire screen and move cursor to top-left
    stdout().flush().unwrap();

    match stats_result {
        Ok(stats) => {
            match cli.format.as_str() {
                "json" => {
                    println!("{}", serde_json::to_string_pretty(&stats)?);
                }
                _ => {
                    display_contribution_graph(&stats);
                    
                    // Enable raw mode for key detection
                    terminal::enable_raw_mode()?;
                    
                    println!();
                    println!("{}", "Press 'q' or Ctrl+C to exit".bright_black());
                    
                    // Keep the process running and listen for key presses
                    loop {
                        if event::poll(Duration::from_millis(100))? {
                            if let CrosstermEvent::Key(key_event) = event::read()? {
                                if key_event.kind == KeyEventKind::Press {
                                    match key_event.code {
                                        KeyCode::Char('q') | KeyCode::Char('Q') => {
                                            break;
                                        }
                                        KeyCode::Char('c') if key_event.modifiers.contains(KeyModifiers::CONTROL) => {
                                            break;
                                        }
                                        KeyCode::Esc => {
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    
                    // Disable raw mode before exiting
                    terminal::disable_raw_mode()?;
                }
            }
        }
        Err(e) => {
            eprintln!("{} {}", "❌ Error:".bright_red(), e);
            std::process::exit(1);
        }
    }

    Ok(())
}
