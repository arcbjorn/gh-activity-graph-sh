use std::io::{stdout, Write};
use std::time::Duration;

use anyhow::{Result, Context};
use chrono::{Utc, Datelike};
use clap::Parser;
use colored::*;
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind},
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

    
    async fn get_contribution_graph_from_graphql(&self) -> Result<ContributionGraph> {
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
        let calendar = graphql_response.data.user.contributions_collection.contribution_calendar;
        
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
        
        Ok(ContributionGraph {
            weeks,
            total_contributions: calendar.total_contributions,
        })
    }
    
    async fn generate_contribution_graph(&self) -> Result<ContributionGraph> {
        match self.get_contribution_graph_from_graphql().await {
            Ok(graph) => Ok(graph),
            Err(_) => {
                Ok(ContributionGraph {
                    weeks: Vec::new(),
                    total_contributions: 0,
                })
            }
        }
    }


    async fn get_stats(&self) -> Result<Stats> {
        let user = self.get_user().await?;
        let contribution_graph = self.generate_contribution_graph().await?;

        Ok(Stats {
            username: user.login,
            contribution_graph,
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
    println!("{} contributions in the last year", stats.contribution_graph.total_contributions.to_string().bright_green());
    
    // Legend
    println!();
    print!("Less ");
    print!("{} ", "⬛".bright_black());
    print!("{} ", "🟩".bright_green());
    print!("{} ", "🟨".bright_yellow());
    print!("{} ", "🟧".bright_yellow());
    print!("{} ", "🟥".bright_red());
    println!("More");
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