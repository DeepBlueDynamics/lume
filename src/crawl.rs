use std::fs;
use std::io::{self, Write};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
struct CrawlPayload<'a> {
    url: &'a str,
    javascript_enabled: bool,
}

#[derive(Deserialize)]
struct CrawlResponse {
    markdown: Option<String>,
    markdown_plain: Option<String>,
    content: Option<String>,
    title: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct HnItem {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    by: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    score: i64,
    #[serde(default)]
    kids: Vec<u64>,
    #[serde(rename = "type", default)]
    item_type: String,
}

fn clean_hn_html(html: &str) -> String {
    let mut cleaned = html
        .replace("<p>", "\n\n")
        .replace("</p>", "")
        .replace("<i>", "*")
        .replace("</i>", "*")
        .replace("<pre><code>", "\n```\n")
        .replace("</code></pre>", "\n```\n")
        .replace("<code>", "`")
        .replace("</code>", "`")
        .replace("<a href=\"", "")
        .replace("\" rel=\"nofollow\">", " ")
        .replace("</a>", "");
    
    // Quick unescapes for common HTML entities
    cleaned = cleaned
        .replace("&#x2F;", "/")
        .replace("&#x27;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    
    cleaned
}

fn extract_hn_id(url: &str) -> Option<String> {
    if url.contains("news.ycombinator.com/item") {
        if let Some(pos) = url.find("id=") {
            let id_part = &url[pos + 3..];
            let end_pos = id_part.find('&').unwrap_or(id_part.len());
            let id = &id_part[..end_pos];
            if id.chars().all(|c| c.is_ascii_digit()) {
                return Some(id.to_string());
            }
        }
    }
    None
}

pub fn crawl_hn_via_api(id: &str) -> Result<(String, String), String> {
    println!("  ➔ Hacker News URL detected. Fetching story and discussions via public Firebase API...");
    io::stdout().flush().unwrap();

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();

    let story_url = format!("https://hacker-news.firebaseio.com/v0/item/{}.json", id);
    let res = agent.get(&story_url)
        .call()
        .map_err(|e| format!("Failed to fetch Hacker News story {}: {}", id, e))?;

    let story: HnItem = res.into_json()
        .map_err(|e| format!("Failed to parse Hacker News story JSON: {}", e))?;

    let title = if story.title.is_empty() {
        format!("Hacker News Post {}", id)
    } else {
        story.title.clone()
    };

    let mut markdown = String::new();
    markdown.push_str(&format!("# {}\n\n", title));
    markdown.push_str(&format!("*   **Source URL**: https://news.ycombinator.com/item?id={}\n", id));
    markdown.push_str(&format!("*   **Author**: {}\n", story.by));
    markdown.push_str(&format!("*   **Points**: {}\n", story.score));
    markdown.push_str(&format!("*   **Crawl Timestamp**: {}\n\n", chrono_timestamp()));
    markdown.push_str("---\n\n");

    if !story.text.is_empty() {
        markdown.push_str(&format!("{}\n\n", clean_hn_html(&story.text)));
        markdown.push_str("---\n\n");
    }

    if !story.kids.is_empty() {
        markdown.push_str("## Top Discussion Comments\n\n");
        let limit = 8;
        let mut comment_count = 0;
        for &kid_id in story.kids.iter().take(limit) {
            let kid_url = format!("https://hacker-news.firebaseio.com/v0/item/{}.json", kid_id);
            if let Ok(k_res) = agent.get(&kid_url).call() {
                if let Ok(comment) = k_res.into_json::<HnItem>() {
                    if !comment.text.is_empty() && !comment.by.is_empty() {
                        comment_count += 1;
                        markdown.push_str(&format!("### Comment by {}\n", comment.by));
                        markdown.push_str(&format!("{}\n\n", clean_hn_html(&comment.text)));
                    }
                }
            }
        }
        if comment_count == 0 {
            markdown.push_str("*(No top-level comments fetched)*\n");
        }
    } else {
        markdown.push_str("*(No comments on this post yet)*\n");
    }

    // Save to personal search engine (examples/crawled/)
    let save_dir = "examples/crawled";
    if let Err(e) = fs::create_dir_all(save_dir) {
        return Err(format!("Failed to create directory '{}': {}", save_dir, e));
    }

    let filename = format!("{}/hn_{}.md", save_dir, id);
    fs::write(&filename, &markdown)
        .map_err(|e| format!("Failed to write markdown file to '{}': {}", filename, e))?;

    Ok((filename, markdown))
}

pub fn crawl_url(url: &str) -> Result<(String, String), String> {
    if let Some(hn_id) = extract_hn_id(url) {
        return crawl_hn_via_api(&hn_id);
    }

    let api_url = std::env::var("GRUB_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:6792".to_string());
    
    let token = load_nuts_token();
    let is_local = api_url.contains("localhost") 
        || api_url.contains("127.0.0.1") 
        || api_url.contains("host.docker.internal") 
        || api_url.contains("172.");

    if is_local {
        println!("  ➔ Dispatching stealth crawl agent to local Grub ({api_url})...");
    } else {
        println!("  ➔ Dispatching stealth crawl agent to remote Grub ({api_url})...");
    }
    io::stdout().flush().unwrap();

    if token.is_none() && !is_local {
        println!("  ⚠️  NUTS_SERVICES_TOKEN not set and remote server detected. Falling back to direct HTTP GET (no JavaScript evaluation)...");
        io::stdout().flush().unwrap();
        return crawl_direct_get(url);
    }

    let endpoint = format!("{}/api/markdown", api_url.trim_end_matches('/'));
    let payload = CrawlPayload {
        url,
        javascript_enabled: true,
    };

    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(60))
        .build();

    let mut req = agent.post(&endpoint)
        .set("Content-Type", "application/json");

    if let Some(ref tok) = token {
        req = req.set("Authorization", &format!("Bearer {}", tok));
    }

    match req.send_json(&payload) {
        Ok(res) => {
            let status = res.status();
            if status != 200 {
                let err_body = res.into_string().unwrap_or_else(|_| "Unknown error".to_string());
                return Err(format!("Crawl failed (Status {}): {}", status, err_body));
            }

            let response_data: CrawlResponse = res.into_json()
                .map_err(|e| format!("Failed to parse response JSON: {}", e))?;

            if let Some(err) = response_data.error {
                return Err(format!("Crawl server error: {}", err));
            }

            let markdown = response_data.markdown
                .or(response_data.markdown_plain)
                .or(response_data.content);

            let content = match markdown {
                Some(c) => c,
                None => return Err("Crawl returned empty content.".to_string()),
            };

            let save_dir = "examples/crawled";
            if let Err(e) = fs::create_dir_all(save_dir) {
                return Err(format!("Failed to create directory '{}': {}", save_dir, e));
            }

            let safe_slug = make_safe_slug(url);
            let filename = format!("{}/{}.md", save_dir, safe_slug);
            let page_title = response_data.title.unwrap_or_else(|| "Crawled Document".to_string());

            let file_content = format!(
                "# {}\n\n*   **Source URL**: {}\n*   **Crawl Timestamp**: {}\n\n---\n\n{}",
                page_title, url, chrono_timestamp(), content
            );

            fs::write(&filename, &file_content)
                .map_err(|e| format!("Failed to write markdown file to '{}': {}", filename, e))?;

            Ok((filename, file_content))
        }
        Err(e) => {
            if is_local && api_url.contains("localhost") {
                println!("  ➔ Localhost connection failed. Retrying crawl via host.docker.internal:6792...");
                io::stdout().flush().unwrap();
                let retry_url = "http://host.docker.internal:6792";
                let retry_endpoint = format!("{}/api/markdown", retry_url);
                let mut retry_req = agent.post(&retry_endpoint)
                    .set("Content-Type", "application/json");
                if let Some(ref tok) = token {
                    retry_req = retry_req.set("Authorization", &format!("Bearer {}", tok));
                }
                if let Ok(res) = retry_req.send_json(&payload) {
                    if res.status() == 200 {
                        if let Ok(response_data) = res.into_json::<CrawlResponse>() {
                            if let Some(content) = response_data.markdown.or(response_data.markdown_plain).or(response_data.content) {
                                let save_dir = "examples/crawled";
                                let _ = fs::create_dir_all(save_dir);
                                let safe_slug = make_safe_slug(url);
                                let filename = format!("{}/{}.md", save_dir, safe_slug);
                                let page_title = response_data.title.unwrap_or_else(|| "Crawled Document".to_string());
                                let file_content = format!(
                                    "# {}\n\n*   **Source URL**: {}\n*   **Crawl Timestamp**: {}\n\n---\n\n{}",
                                    page_title, url, chrono_timestamp(), content
                                );
                                let _ = fs::write(&filename, &file_content);
                                return Ok((filename, file_content));
                            }
                        }
                    }
                }
            }
            println!("  ⚠️  Crawler connection failed ({}). Falling back to direct HTTP GET...", e);
            io::stdout().flush().unwrap();
            crawl_direct_get(url)
        }
    }
}

fn crawl_direct_get(url: &str) -> Result<(String, String), String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(15))
        .build();

    let res = agent.get(url)
        .call()
        .map_err(|e| format!("Direct fallback connection failed: {}", e))?;

    let status = res.status();
    if status != 200 {
        return Err(format!("Direct fallback request failed (Status {}).", status));
    }

    let content = res.into_string()
        .map_err(|e| format!("Failed to parse direct response body: {}", e))?;

    let save_dir = "examples/crawled";
    if let Err(e) = fs::create_dir_all(save_dir) {
        return Err(format!("Failed to create directory '{}': {}", save_dir, e));
    }

    let safe_slug = make_safe_slug(url);
    let filename = format!("{}/{}.html", save_dir, safe_slug);

    let file_content = format!(
        "<!-- Source URL: {} -->\n<!-- Timestamp: {} -->\n{}",
        url,
        chrono_timestamp(),
        content
    );

    fs::write(&filename, &file_content)
        .map_err(|e| format!("Failed to write direct crawl content to '{}': {}", filename, e))?;

    Ok((filename, file_content))
}

pub fn run(args: Vec<String>) {
    if args.is_empty() {
        println!("\x1B[1;31mError: No URL provided.\x1B[0m");
        println!("\x1B[1;33mUsage:\x1B[0m");
        println!("  lume crawl <URL>");
        println!();
        println!("\x1B[1;33mExample:\x1B[0m");
        println!("  lume crawl https://example.com");
        return;
    }

    let url = &args[0];

    println!("\x1B[1;36m🕷️  Lume Crawler starting for: {}\x1B[0m", url);
    io::stdout().flush().unwrap();

    let start = std::time::Instant::now();

    match crawl_url(url) {
        Ok((filename, _)) => {
            let elapsed = start.elapsed();
            println!("\x1B[1;32m✓ Crawled successfully in {:.2?}!\x1B[0m", elapsed);
            println!(
                "\x1B[32mSuccessfully added to personal search engine document collection!\x1B[0m"
            );
            println!("  ➔ Saved to: \x1B[1;34m{}\x1B[0m", filename);
            println!("  ➔ You can now search it immediately using: \x1B[1;36mlume search examples/crawled \"your query\"\x1B[0m");
        }
        Err(e) => {
            eprintln!("\x1B[1;31mError: {}\x1B[0m", e);
            std::process::exit(1);
        }
    }
}

fn load_nuts_token() -> Option<String> {
    // 1. Check environment variable
    if let Ok(tok) = std::env::var("NUTS_SERVICES_TOKEN") {
        return Some(tok.trim().to_string());
    }
    // 2. Read .env file in current directory
    if let Ok(content) = fs::read_to_string(".env") {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with("NUTS_SERVICES_TOKEN=") {
                let parts: Vec<&str> = line.splitn(2, '=').collect();
                if parts.len() == 2 {
                    return Some(parts[1].trim().to_string());
                }
            }
        }
    }
    None
}

fn make_safe_slug(url: &str) -> String {
    let stripped = url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let mut slug = String::new();
    for c in stripped.chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if c == '/' || c == '?' || c == '&' || c == '=' || c == '-' || c == '_' || c == '.' {
            slug.push('_');
        }
    }
    // Remove repeated underscores
    let mut cleaned = String::new();
    let mut last_was_underscore = false;
    for c in slug.chars() {
        if c == '_' {
            if !last_was_underscore {
                cleaned.push('_');
                last_was_underscore = true;
            }
        } else {
            cleaned.push(c);
            last_was_underscore = false;
        }
    }
    let trimmed = cleaned.trim_matches('_');
    if trimmed.is_empty() {
        "index".to_string()
    } else {
        trimmed.to_string()
    }
}

fn chrono_timestamp() -> String {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => {
            let secs = d.as_secs();
            format!("Unix Epoch Secs {}", secs)
        }
        Err(_) => "Unknown time".to_string(),
    }
}
