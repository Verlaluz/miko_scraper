use crate::services::{AccountService, ScrapingService};
use crate::models::ScrapingRecord;
use crate::services::scraping_service::AccountVoteStatus;
use std::collections::BTreeMap;
use std::sync::Arc;
use teloxide::{
    dispatching::dialogue::InMemStorage,
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup},
    utils::command::BotCommands,
};

/// Type alias for dialogue management in memory.
pub type MyDialogue = Dialogue<State, InMemStorage<State>>;
pub type HandlerResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

/// Finite State Machine defining where the user is in the conversation.
#[derive(Clone, Default, Debug)]
pub enum State {
    #[default]
    Start,
    AwaitingPhoneNumber,
    AwaitingWorkerName,
    AwaitingAccountSearch,
    AwaitingOtp {
        phone_number: String,
    },
}

/// Commands accepted by the Telegram Bot.
#[derive(BotCommands, Clone, Debug)]
#[command(rename_rule = "lowercase", description = "Available commands:")]
pub enum Command {
    #[command(description = "Start the bot and view menu.")]
    Start,
    #[command(description = "Register a new phone number with OTP.")]
    Register,
    #[command(description = "Become a worker and track your registered accounts.")]
    Worker,
    #[command(description = "Register a worker: /adminregister <telegram_id> <name>.")]
    Adminregister(String),
    #[command(description = "Show worker account counts.")]
    Admin,
    #[command(description = "Show new accounts from the last 12 hours for payment.")]
    Pay,
    #[command(description = "Show new and updated worker accounts.")]
    Check,
    #[command(description = "Show 20-minute operational activity report.")]
    Rep,
    #[command(description = "View real-time voting eligibility for all accounts.")]
    Status,
    #[command(description = "Show accounts eligible to vote now.")]
    Eligible,
    #[command(description = "Show accounts in voting cooldown.")]
    Cooldown,
    #[command(description = "Show bot and database health.")]
    Health,
    #[command(description = "Cancel current operation and return to start.")]
    Cancel,
    #[command(description = "Help and command list.")]
    Help,
}

/// Starts the Teloxide dispatcher in a background Tokio task with automatic reconnects.
pub fn start_telegram_bot(
    token: String,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    admin_telegram_ids: Vec<i64>,
) {
    tokio::spawn(async move {
        let storage = InMemStorage::<State>::new();
        loop {
            tracing::info!("Initializing Telegram Bot dispatcher...");
            let bot = Bot::new(token.clone());

            let handler = dptree::entry()
                .branch(                Update::filter_callback_query()
                    .enter_dialogue::<CallbackQuery, InMemStorage<State>, State>()
                    .endpoint(handle_status_filter))
                .branch(
                    Update::filter_message()
                        .enter_dialogue::<Message, InMemStorage<State>, State>()
                        .branch(
                            dptree::entry()
                                .filter_command::<Command>()
                                .endpoint(handle_command),
                        )
                        .branch(dptree::case![State::Start].endpoint(handle_start_text))
                        .branch(dptree::case![State::AwaitingPhoneNumber].endpoint(handle_phone_number))
                        .branch(dptree::case![State::AwaitingWorkerName].endpoint(handle_worker_name))
                        .branch(dptree::case![State::AwaitingAccountSearch].endpoint(handle_account_search))
                        .branch(
                            dptree::case![State::AwaitingOtp { phone_number }]
                                .endpoint(handle_otp_code),
                        ),
                );

            let mut dispatcher = Dispatcher::builder(bot, handler)
                .dependencies(dptree::deps![
                    storage.clone(),
                    Arc::clone(&account_service),
                    Arc::clone(&scraping_service),
                    admin_telegram_ids.clone()
                ])
                .enable_ctrlc_handler()
                .build();

            tracing::info!("Telegram Bot listener is now active!");
            dispatcher.dispatch().await;

            tracing::warn!("Telegram Bot dispatcher exited. Reconnecting in 5 seconds...");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
}

/// Handles slash commands like /start, /register, /status, /cancel.
async fn handle_command(
    bot: Bot,
    dialogue: MyDialogue,
    cmd: Command,
    msg: Message,
    scraping_service: Arc<ScrapingService>,
    account_service: Arc<AccountService>,
    admin_telegram_ids: Vec<i64>,
) -> HandlerResult {
    match cmd {
        Command::Start | Command::Help => {
            dialogue.update(State::Start).await?;
            let welcome = "👋 <b>Welcome to the CreativeAwards Automated Voter Bot!</b>\n\n\
                Use this bot to register phone accounts, verify OTPs, and track 12-hour votes for family & friends.\n\n\
                <b>Commands:</b>\n\
                /register — Add a phone number & receive OTP\n\
                /worker — Become a worker and track your accounts\n\
                /admin — Show worker account counts (admins only)\n\
                /pay — Show new accounts from the last 12 hours for payment (admins only)\n\
                /check — Show new and updated worker accounts (admins only)\n\
                /rep — Show 20-minute operational report (admins only)\n\
                /cancel — Cancel current action\n\
                /help — Show this help message";
            bot.send_message(msg.chat.id, welcome)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
        Command::Status if !is_admin(&msg, &admin_telegram_ids) => {
            bot.send_message(msg.chat.id, "⛔ The /status command is available to administrators only.")
                .await?;
        }
        Command::Eligible | Command::Cooldown | Command::Health | Command::Rep
            if !is_admin(&msg, &admin_telegram_ids) =>
        {
            bot.send_message(
                msg.chat.id,
                "⛔ This command is available to administrators only.",
            )
            .await?;
        }
        Command::Register => {
            dialogue.update(State::AwaitingPhoneNumber).await?;
            bot.send_message(
                msg.chat.id,
                "📱 <b>Please enter the Ethiopian phone number:</b>\n\
                (Format: <code>0912345678</code> or <code>+251912345678</code>)\n\n\
                Send /cancel at any time to exit.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        Command::Worker => {
            dialogue.update(State::AwaitingWorkerName).await?;
            bot.send_message(
                msg.chat.id,
                "👷 Please enter your name. This name will appear in the administrators' worker report.",
            )
            .await?;
        }
        Command::Adminregister(args) => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to register workers.")
                    .await?;
            } else {
                let mut parts = args.splitn(2, char::is_whitespace);
                let telegram_id = parts.next().and_then(|value| value.parse::<i64>().ok());
                let name = parts.next().map(str::trim).filter(|value| !value.is_empty());
                match (telegram_id, name) {
                    (Some(telegram_id), Some(name)) => {
                        account_service.register_worker(telegram_id, name).await?;
                        bot.send_message(
                            msg.chat.id,
                            format!("✅ Worker <b>{}</b> ({}) is now registered.", name, telegram_id),
                        )
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await?;
                    }
                    _ => {
                        bot.send_message(
                            msg.chat.id,
                            "Usage: <code>/adminregister 123456789 Worker Name</code>",
                        )
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await?;
                    }
                }
            }
        }
        Command::Admin => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to view worker statistics.")
                    .await?;
            } else {
                let workers = account_service.worker_statistics().await?;
                let report = if workers.is_empty() {
                    "No workers are registered yet.".to_string()
                } else {
                    let rows = workers
                        .iter()
                        .enumerate()
                        .map(|(index, worker)| {
                            format!(
                                "{}. <b>{}</b> — {} account(s), {} cooldown (<code>{}</code>)",
                                index + 1,
                                worker.display_name,
                                worker.account_count,
                                worker.cooldown_count,
                                worker.telegram_id
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!("📊 <b>Worker account totals</b>\n\n{}", rows)
                };
                bot.send_message(msg.chat.id, report)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(InlineKeyboardMarkup::new(vec![vec![
                        InlineKeyboardButton::callback(
                            "📩 Pending OTP accounts",
                            "admin:pending_otp",
                        ),
                    ]]))
                    .await?;
            }
        }
        Command::Pay => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to view payment statistics.")
                    .await?;
            } else {
                let workers = account_service.payment_statistics().await?;
                let report = if workers.is_empty() {
                    "No new accounts registered in the last 12 hours.".to_string()
                } else {
                    let rows = workers
                        .iter()
                        .enumerate()
                        .map(|(index, worker)| {
                            format!(
                                "{}. <b>{}</b> — {} account(s), {} cooldown\n(<code>{}</code>)",
                                index + 1,
                                worker.display_name,
                                worker.account_count,
                                worker.cooldown_count,
                                worker.telegram_id
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    format!(
                        "💰 <b>Worker payment accounts (new registrations in the last 12 hours)</b>\n\n{}",
                        rows
                    )
                };
                if report.len() <= 3800 {
                    bot.send_message(msg.chat.id, report)
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await?;
                } else {
                    let mut chunk = String::new();
                    for line in report.lines() {
                        if chunk.len() + line.len() + 1 > 3800 {
                            bot.send_message(msg.chat.id, &chunk)
                                .parse_mode(teloxide::types::ParseMode::Html)
                                .await?;
                            chunk.clear();
                        }
                        if !chunk.is_empty() {
                            chunk.push('\n');
                        }
                        chunk.push_str(line);
                    }
                    if !chunk.is_empty() {
                        bot.send_message(msg.chat.id, chunk)
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await?;
                    }
                }
            }
        }
        Command::Check => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to use /check.")
                    .await?;
            } else {
                bot.send_message(
                    msg.chat.id,
                    "🔎 <b>Worker account check (since 00:00 UTC)</b>\n\nChoose a report:",
                )
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(InlineKeyboardMarkup::new(vec![vec![
                    InlineKeyboardButton::callback("🆕 New accounts", "admin:check_new"),
                    InlineKeyboardButton::callback("🔄 Updated accounts", "admin:check_updated"),
                ]]))
                .await?;
            }
        }
        Command::Rep => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to view this report.")
                    .await?;
            } else {
                let report_data = account_service.activity_report_20m().await?;
                let start_utc = report_data.start_time.format("%H:%M");
                let end_utc = report_data.end_time.format("%H:%M");
                let eat_offset = chrono::FixedOffset::east_opt(3 * 3600).unwrap();
                let start_eat = report_data.start_time.with_timezone(&eat_offset).format("%H:%M");
                let end_eat = report_data.end_time.with_timezone(&eat_offset).format("%H:%M");

                let mut text = format!(
                    "📊 <b>Operational Activity Report (20 Minutes)</b>\n\n\
                    ⏱ <b>Period:</b>\n\
                    • <b>UTC:</b> <code>{}</code> — <code>{}</code>\n\
                    • <b>EAT:</b> <code>{}</code> — <code>{}</code>\n\n\
                    📱 <b>New Registered Accounts:</b> {}\n\
                    ⚡ <b>Claimed Accounts (Status 201):</b> {}\n",
                    start_utc,
                    end_utc,
                    start_eat,
                    end_eat,
                    report_data.new_accounts_count,
                    report_data.claimed_accounts_count
                );

                if !report_data.worker_claim_stats.is_empty() {
                    text.push_str("\n👷 <b>Worker Activity (Status 201 Claims):</b>\n");
                    for (i, worker) in report_data.worker_claim_stats.iter().enumerate() {
                        let id_str = worker
                            .telegram_id
                            .map(|id| format!(" (<code>{}</code>)", id))
                            .unwrap_or_default();
                        text.push_str(&format!(
                            "{}. <b>{}</b>{} — {} claimed\n",
                            i + 1,
                            worker.display_name,
                            id_str,
                            worker.claimed_accounts
                        ));
                    }
                }

                if text.len() <= 3800 {
                    bot.send_message(msg.chat.id, text)
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await?;
                } else {
                    let mut chunk = String::new();
                    for line in text.lines() {
                        if chunk.len() + line.len() + 1 > 3800 {
                            bot.send_message(msg.chat.id, &chunk)
                                .parse_mode(teloxide::types::ParseMode::Html)
                                .await?;
                            chunk.clear();
                        }
                        if !chunk.is_empty() {
                            chunk.push('\n');
                        }
                        chunk.push_str(line);
                    }
                    if !chunk.is_empty() {
                        bot.send_message(msg.chat.id, chunk)
                            .parse_mode(teloxide::types::ParseMode::Html)
                            .await?;
                    }
                }
            }
        }
        Command::Status | Command::Eligible | Command::Cooldown => {
            let accounts = account_service.list_accounts().await?;
            if accounts.is_empty() {
                bot.send_message(
                    msg.chat.id,
                    "ℹ️ No accounts registered yet. Use /register to add your first phone number!",
                )
                .await?;
                return Ok(());
            }

            let statuses = scraping_service.get_all_accounts_vote_status(&accounts).await?;
            let filtered_statuses: Vec<AccountVoteStatus> = match &cmd {
                Command::Eligible => statuses
                    .iter()
                    .filter(|status| status.is_eligible)
                    .cloned()
                    .collect(),
                Command::Cooldown => statuses
                    .iter()
                    .filter(|status| !status.is_eligible)
                    .cloned()
                    .collect(),
                Command::Status => statuses.clone(),
                _ => unreachable!("command is matched above"),
            };
            let show_filters = matches!(&cmd, Command::Status);
            send_status_report(
                &bot,
                msg.chat.id,
                &filtered_statuses,
                Some(statuses.len()),
                show_filters,
                match &cmd {
                    Command::Eligible => "eligible",
                    Command::Cooldown => "cooldown",
                    _ => "all",
                },
                0,
                None,
            )
            .await?;
        }
        Command::Health => {
            let accounts = account_service.list_accounts().await?;
            bot.send_message(
                msg.chat.id,
                format!(
                    "✅ <b>Miko Scraper is healthy</b>\n\nTelegram polling: active\nDatabase: connected\nRegistered accounts: {}",
                    accounts.len()
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        Command::Cancel => {
            dialogue.update(State::Start).await?;
            bot.send_message(msg.chat.id, "❌ Action cancelled. Returned to main menu. Type /register to start again.")
                .await?;
        }
    }
    Ok(())
}

fn is_admin(msg: &Message, admin_telegram_ids: &[i64]) -> bool {
    msg.from
        .as_ref()
        .map(|user| admin_telegram_ids.contains(&(user.id.0 as i64)))
        .unwrap_or(false)
}

async fn handle_worker_name(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    account_service: Arc<AccountService>,
) -> HandlerResult {
    let Some(name) = msg.text().map(str::trim).filter(|name| !name.is_empty()) else {
        bot.send_message(msg.chat.id, "Please enter a valid name, or use /cancel.")
            .await?;
        return Ok(());
    };

    let Some(user) = msg.from.as_ref() else {
        bot.send_message(msg.chat.id, "I could not identify your Telegram account. Please try again.")
            .await?;
        return Ok(());
    };

    account_service
        .register_worker(user.id.0 as i64, name)
        .await?;
    dialogue.update(State::Start).await?;
    bot.send_message(
        msg.chat.id,
        format!(
            "🎉 Congratulations, {}! You are now registered as a worker.\n\nYour Telegram ID: <code>{}</code>\nUse /register to add phone accounts. The administrators will see your account total in /admin.",
            name, user.id.0
        ),
    )
    .parse_mode(teloxide::types::ParseMode::Html)
    .await?;
    Ok(())
}

async fn send_status_report(
    bot: &Bot,
    chat_id: ChatId,
    statuses: &[AccountVoteStatus],
    total_count: Option<usize>,
    show_filters: bool,
    filter: &str,
    page: usize,
    range: Option<usize>,
) -> HandlerResult {
    let eligible_count = statuses.iter().filter(|status| status.is_eligible).count();
    let cooldown_count = statuses.len() - eligible_count;
    let total_count = total_count.unwrap_or(statuses.len());
    let mut report = format!(
        "📊 Family & Friends Vote Eligibility\n\n\
         Showing: {} of {} accounts\n\
         🟢 Eligible now: {}\n\
         🟡 Already voted / cooldown: {}\n",
        statuses.len(),
        total_count,
        eligible_count,
        cooldown_count,
    );

    let page_size = 20;
    let visible_statuses = match range {
        Some(10) => &statuses[..statuses.len().min(10)],
        Some(20) => &statuses[..statuses.len().min(20)],
        Some(100) => &statuses[..statuses.len().min(100)],
        _ => statuses,
    };
    let page_count = ((visible_statuses.len() + page_size - 1) / page_size).max(1);
    let page = page.min(page_count - 1);
    let start = page * page_size;
    let end = (start + page_size).min(visible_statuses.len());
    report.push_str(&format!("\nPage {} of {}\n", page + 1, page_count));
    for status in &visible_statuses[start..end] {
        let label = status.label.as_deref().unwrap_or("Account");
        report.push_str(&format!("• {} #{}\n", label, status.account_id));
    }

    let chunks = report
        .lines()
        .fold(Vec::<String>::new(), |mut chunks, line| {
            const MAX_MESSAGE_LENGTH: usize = 3800;
            if chunks
                .last()
                .map(|chunk| chunk.len() + line.len() + 1 <= MAX_MESSAGE_LENGTH)
                .unwrap_or(false)
            {
                chunks.last_mut().unwrap().push('\n');
                chunks.last_mut().unwrap().push_str(line);
            } else {
                chunks.push(line.to_string());
            }
            chunks
        });

    for (index, chunk) in chunks.into_iter().enumerate() {
        let mut message = bot.send_message(chat_id, chunk);
        if index == 0 {
            message = message.reply_markup(status_keyboard(
                &visible_statuses[start..end],
                filter,
                page,
                page_count,
                show_filters,
                range,
            ));
        }
        message.await?;
    }

    Ok(())
}

fn status_keyboard(
    statuses: &[AccountVoteStatus],
    filter: &str,
    page: usize,
    page_count: usize,
    show_filters: bool,
    range: Option<usize>,
) -> InlineKeyboardMarkup {
    let mut rows = if range.is_some() {
        statuses
            .chunks(2)
            .map(|row| {
                row.iter()
                    .map(|status| {
                        InlineKeyboardButton::callback(
                            status.phone_number.clone(),
                            format!("account:{}", status.account_id),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    if page_count > 1 {
        let mut navigation = Vec::new();
        if page > 0 {
            navigation.push(InlineKeyboardButton::callback(
                "⬅️ Previous",
                format!("status:{}:{}:{}", filter, page - 1, range.unwrap_or(0)),
            ));
        }
        if page + 1 < page_count {
            navigation.push(InlineKeyboardButton::callback(
                "Next ➡️",
                format!("status:{}:{}:{}", filter, page + 1, range.unwrap_or(0)),
            ));
        }
        rows.push(navigation);
    }
    if show_filters && range.is_none() {
        rows.push(vec![
            InlineKeyboardButton::callback("Recent 10", "range:10"),
            InlineKeyboardButton::callback("Recent 20", "range:20"),
            InlineKeyboardButton::callback("Past 100", "range:100"),
        ]);
        rows.push(vec![
            InlineKeyboardButton::callback("⏱️ ≤30 minutes", "cooldown:30"),
            InlineKeyboardButton::callback("⏱️ 30–60 minutes", "cooldown:60"),
        ]);
        rows.push(vec![InlineKeyboardButton::callback(
            "🔎 Search account",
            "search:account",
        )]);
    }
    if range.is_some() {
        rows.push(vec![InlineKeyboardButton::callback("⬅️ Back to ranges", "range:0")]);
    }
    InlineKeyboardMarkup::new(rows)
}

async fn handle_status_filter(
    bot: Bot,
    dialogue: MyDialogue,
    query: CallbackQuery,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    admin_telegram_ids: Vec<i64>,
) -> HandlerResult {
    if let Some(data) = query.data.clone() {
        if data == "admin:check_new" || data == "admin:check_updated" {
            if !is_admin_callback(&query, &admin_telegram_ids) {
                bot.answer_callback_query(query.id)
                    .text("Admins only.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            }
            return send_worker_check_report(
                bot,
                query,
                account_service,
                scraping_service,
                data == "admin:check_new",
            )
            .await;
        }
    }
    if data_is_admin_pending_otp(&query, &admin_telegram_ids) {
        bot.answer_callback_query(query.id).await?;
        let pending = account_service.pending_otp_accounts().await?;
        let report_lines = if pending.is_empty() {
            vec!["✅ No accounts are currently pending OTP verification.".to_string()]
        } else {
            let rows = pending
                .iter()
                .enumerate()
                .map(|(index, account)| {
                    let worker = account
                        .worker_name
                        .as_deref()
                        .unwrap_or("Unassigned worker");
                    let worker_id = account
                        .worker_telegram_id
                        .map(|id| format!(" ({})", id))
                        .unwrap_or_default();
                    format!(
                        "{}. <code>{}</code> — {}{}",
                        index + 1,
                        account.phone_number,
                        worker,
                        worker_id
                    )
                })
                .collect::<Vec<_>>();
            let mut lines = vec![format!("📩 <b>Pending OTP accounts: {}</b>", pending.len())];
            lines.extend(rows);
            lines
        };
        if let Some(message) = query.message {
            let mut chunk = String::new();
            for line in report_lines {
                if chunk.len() + line.len() + 1 > 3800 {
                    bot.send_message(message.chat().id, &chunk)
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await?;
                    chunk.clear();
                }
                if !chunk.is_empty() {
                    chunk.push('\n');
                }
                chunk.push_str(&line);
            }
            if !chunk.is_empty() {
                bot.send_message(message.chat().id, chunk)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
            }
        }
        return Ok(());
    }
    if !admin_telegram_ids.contains(&(query.from.id.0 as i64)) {
        bot.answer_callback_query(query.id)
            .text("Status controls are available to administrators only.")
            .show_alert(true)
            .await?;
        return Ok(());
    }

    let data = query.data.clone().unwrap_or_else(|| "status:all:0".to_string());
    if let Some(value) = data.strip_prefix("cooldown:") {
        let maximum_minutes = value.parse::<i64>().ok().unwrap_or(30);
        return handle_cooldown_filter(
            bot,
            query,
            account_service,
            scraping_service,
            maximum_minutes,
        )
        .await;
    }
    if data == "search:account" {
        bot.answer_callback_query(query.id).await?;
        dialogue.update(State::AwaitingAccountSearch).await?;
        if let Some(message) = query.message {
            bot.send_message(
                message.chat().id,
                "🔎 Send the registered phone number to search.\nExample: <code>0946692896</code>",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        return Ok(());
    }
    if let Some(value) = data.strip_prefix("range:") {
        return handle_status_range(
            bot,
            query,
            account_service,
            scraping_service,
            value.parse::<usize>().ok(),
        )
        .await;
    }
    if let Some(account_id) = data.strip_prefix("account:").and_then(|id| id.parse().ok()) {
        return handle_account_detail(bot, query, account_service, scraping_service, account_id).await;
    }
    let mut parts = data.split(':');
    let _ = parts.next();
    let filter = parts.next().unwrap_or("all");
    let page = parts.next().and_then(|value| value.parse().ok()).unwrap_or(0);
    let range = parts
        .next()
        .and_then(|value| value.parse().ok())
        .filter(|value| matches!(value, 10 | 20 | 100));

    bot.answer_callback_query(query.id).await?;

    let Some(message) = query.message else {
        return Ok(());
    };

    let accounts = account_service.list_accounts().await?;
    let statuses = scraping_service
        .get_all_accounts_vote_status(&accounts)
        .await?;
    let filtered: Vec<AccountVoteStatus> = match filter {
        "eligible" => statuses
            .iter()
            .filter(|status| status.is_eligible)
            .cloned()
            .collect(),
        "cooldown" => statuses
            .iter()
            .filter(|status| !status.is_eligible)
            .cloned()
            .collect(),
        _ => statuses.clone(),
    };

    send_status_report(
        &bot,
        message.chat().id,
        &filtered,
        Some(statuses.len()),
        true,
        filter,
        page,
        range,
    )
    .await
}

fn data_is_admin_pending_otp(query: &CallbackQuery, admin_telegram_ids: &[i64]) -> bool {
    query.data.as_deref() == Some("admin:pending_otp")
        && admin_telegram_ids.contains(&(query.from.id.0 as i64))
}

fn is_admin_callback(query: &CallbackQuery, admin_telegram_ids: &[i64]) -> bool {
    admin_telegram_ids.contains(&(query.from.id.0 as i64))
}

async fn send_worker_check_report(
    bot: Bot,
    query: CallbackQuery,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    show_new: bool,
) -> HandlerResult {
    bot.answer_callback_query(query.id).await?;
    let Some(message) = query.message else {
        return Ok(());
    };
    let (new_accounts, updated_accounts) = account_service.worker_check_accounts().await?;
    let accounts = if show_new {
        new_accounts
    } else {
        updated_accounts
    };
    let mut workers = BTreeMap::<String, (i64, i64, i64)>::new();
    for account in &accounts {
        let cooldown = scraping_service.get_cooldown_status(account.id).await?;
        let entry = workers
            .entry(account.worker_name.clone())
            .or_insert((0, 0, 0));
        entry.0 += 1;
        if !cooldown.is_eligible {
            entry.1 += 1;
        }
        entry.2 = account.worker_telegram_id.unwrap_or_default();
    }

    let title = if show_new {
        "🆕 New accounts registered today"
    } else {
        "🔄 Existing accounts updated today"
    };
    let cooldown_count: i64 = workers.values().map(|value| value.1).sum();
    let mut report = format!(
        "<b>{}</b>\nSince 00:00 UTC\n\nTotal: {} account(s)\nCooldown: {}\nEligible: {}\n\n",
        title, accounts.len(), cooldown_count, accounts.len().saturating_sub(cooldown_count as usize)
    );
    if accounts.is_empty() {
        report.push_str("\nNo matching worker accounts.");
    } else {
        for (index, (worker, (count, cooldown, telegram_id))) in workers.iter().enumerate() {
            report.push_str(&format!(
                "{}. <b>{}</b> — {} account(s), {} cooldown\n(<code>{}</code>)\n",
                index + 1,
                worker,
                count,
                cooldown,
                telegram_id
            ));
        }
    }

    for chunk in report
        .as_bytes()
        .chunks(3800)
        .map(|chunk| String::from_utf8_lossy(chunk).into_owned())
    {
        bot.send_message(message.chat().id, chunk)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("🆕 New accounts", "admin:check_new"),
                InlineKeyboardButton::callback("🔄 Updated accounts", "admin:check_updated"),
            ]]))
            .await?;
    }
    Ok(())
}

async fn handle_cooldown_filter(
    bot: Bot,
    query: CallbackQuery,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    maximum_minutes: i64,
) -> HandlerResult {
    bot.answer_callback_query(query.id).await?;
    let Some(message) = query.message else {
        return Ok(());
    };
    let accounts = account_service.list_accounts().await?;
    let statuses = scraping_service.get_all_accounts_vote_status(&accounts).await?;
    let minimum_seconds = if maximum_minutes == 60 { 30 * 60 } else { 0 };
    let maximum_seconds = maximum_minutes * 60;
    let mut filtered: Vec<AccountVoteStatus> = statuses
        .iter()
        .filter(|status| {
            !status.is_eligible
                && status.remaining_seconds > minimum_seconds
                && status.remaining_seconds <= maximum_seconds
        })
        .cloned()
        .collect();
    filtered.sort_by_key(|status| (status.remaining_seconds, status.account_id));

    send_status_report(
        &bot,
        message.chat().id,
        &filtered,
        Some(statuses.len()),
        false,
        "all",
        0,
        Some(100),
    )
    .await
}

async fn handle_status_range(
    bot: Bot,
    query: CallbackQuery,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    range: Option<usize>,
) -> HandlerResult {
    bot.answer_callback_query(query.id).await?;
    let Some(message) = query.message else {
        return Ok(());
    };
    let accounts = account_service.list_accounts().await?;
    let statuses = scraping_service.get_all_accounts_vote_status(&accounts).await?;
    send_status_report(
        &bot,
        message.chat().id,
        &statuses,
        Some(statuses.len()),
        true,
        "all",
        0,
        range.filter(|value| matches!(value, 10 | 20 | 100)),
    )
    .await
}

async fn handle_account_search(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
) -> HandlerResult {
    let input = match msg.text() {
        Some(value) => value.trim(),
        None => {
            bot.send_message(msg.chat.id, "Please send the phone number as text.")
                .await?;
            return Ok(());
        }
    };

    let phone = match AccountService::normalize_phone(input) {
        Ok(phone) => phone,
        Err(_) => {
            bot.send_message(
                msg.chat.id,
                "Invalid phone number. Use the format <code>0946692896</code>.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    };

    let accounts = account_service.list_accounts().await?;
    let statuses = scraping_service.get_all_accounts_vote_status(&accounts).await?;
    let status = statuses.iter().find(|status| status.phone_number == phone);
    dialogue.update(State::Start).await?;

    match status {
        Some(status) => {
            let history = scraping_service.get_history(status.account_id, 1).await?;
            send_account_detail(&bot, msg.chat.id, status, history.first()).await?
        }
        None => {
            bot.send_message(
                msg.chat.id,
                format!("No registered account was found for <code>{}</code>.", phone),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("⬅️ Back to ranges", "range:0"),
            ]]))
            .await?;
        }
    }

    Ok(())
}

async fn handle_account_detail(
    bot: Bot,
    query: CallbackQuery,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
    account_id: i64,
) -> HandlerResult {
    bot.answer_callback_query(query.id).await?;
    let Some(message) = query.message else {
        return Ok(());
    };
    let accounts = account_service.list_accounts().await?;
    let statuses = scraping_service.get_all_accounts_vote_status(&accounts).await?;
    let Some(status) = statuses.iter().find(|status| status.account_id == account_id) else {
        bot.send_message(message.chat().id, "Account not found. Please request /status again.")
            .await?;
        return Ok(());
    };
    let history = scraping_service.get_history(status.account_id, 1).await?;
    send_account_detail(&bot, message.chat().id, status, history.first()).await
}

async fn send_account_detail(
    bot: &Bot,
    chat_id: ChatId,
    status: &AccountVoteStatus,
    latest_record: Option<&ScrapingRecord>,
) -> HandlerResult {
    let status_text = if status.is_eligible {
        "🟢 Eligible (ready to vote)".to_string()
    } else {
        format!(
            "🟡 Already voted / cooldown\nRemaining: {}h {}m",
            status.remaining_seconds / 3600,
            (status.remaining_seconds % 3600) / 60
        )
    };
    let label = status.label.as_deref().unwrap_or("Unnamed");
    let verification = latest_record
        .map(|record| {
            let before = record
                .public_vote_count_before
                .map(|value| value.to_string())
                .unwrap_or_else(|| "Unavailable".to_string());
            let after = record
                .public_vote_count_after
                .map(|value| value.to_string())
                .unwrap_or_else(|| "Unavailable".to_string());
            let delta = match (record.public_vote_count_before, record.public_vote_count_after) {
                (Some(before), Some(after)) => format!(" (Δ {:+})", after - before),
                _ => String::new(),
            };
            format!(
                "\n\n<b>Vote verification</b>\nClaim response: HTTP {}\nPoints recorded: {}\nPublic count: {} → {}{}",
                record.response_status
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "Unavailable".to_string()),
                record.records_scraped,
                before,
                after,
                delta
            )
        })
        .unwrap_or_else(|| "\n\n<b>Vote verification</b>\nNo claim record available.".to_string());
    bot.send_message(
        chat_id,
        format!(
            "👤 <b>Account #{}</b>\n\nLabel: {}\nPhone: <code>{}</code>\nStatus: {}\nLast vote: {}{}",
            status.account_id,
            label,
            status.phone_number,
            status_text,
            status.last_vote_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_else(|| "No recorded vote".to_string()),
            verification
        ),
    )
    .parse_mode(teloxide::types::ParseMode::Html)
    .reply_markup(InlineKeyboardMarkup::new(vec![vec![
        InlineKeyboardButton::callback("⬅️ Back to ranges", "range:0"),
    ]]))
    .await?;
    Ok(())
}

/// Text sent while in Start state.
/// If user pastes or types a phone number directly, automatically starts registration without needing /register!
async fn handle_start_text(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    account_service: Arc<AccountService>,
) -> HandlerResult {
    if let Some(text) = msg.text() {
        let trimmed = text.trim();
        // Check if input is a valid Ethiopian phone number
        if AccountService::normalize_phone(trimmed).is_ok() {
            return handle_phone_number(bot, dialogue, msg, account_service).await;
        }
    }

    bot.send_message(
        msg.chat.id,
        "💡 <b>Tip:</b> Paste any Ethiopian phone number (e.g. <code>0912345678</code> or <code>+251912345678</code>) directly to register it!",
    )
    .parse_mode(teloxide::types::ParseMode::Html)
    .await?;
    Ok(())
}

/// Step 1: User enters a phone number.
async fn handle_phone_number(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    account_service: Arc<AccountService>,
) -> HandlerResult {
    let raw_text = match msg.text() {
        Some(t) => t.trim(),
        None => {
            bot.send_message(msg.chat.id, "Please enter a valid phone number as text.")
                .await?;
            return Ok(());
        }
    };

    bot.send_message(msg.chat.id, "⏳ Sending OTP request to CreativeAwards...").await?;

    match account_service.initiate_registration(raw_text).await {
        Ok(_challenge_id) => {
            if let Some(user) = msg.from.as_ref() {
                account_service
                    .attribute_account(raw_text, user.id.0 as i64)
                    .await?;
            }
            let normalized = account_service.normalize_phone_number(raw_text)?;
            dialogue
                .update(State::AwaitingOtp {
                    phone_number: normalized.clone(),
                })
                .await?;

            bot.send_message(
                msg.chat.id,
                format!(
                    "📩 <b>SMS OTP Sent!</b>\n\n\
                    We sent an official 6-digit verification code to <code>{}</code>.\n\n\
                    <b>Please reply with the 6-digit code:</b>",
                    normalized
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        Err(crate::errors::AppError::AccountAlreadyExists(msg_str)) => {
            dialogue.update(State::Start).await?;
            bot.send_message(
                msg.chat.id,
                format!("ℹ️ <b>Already Registered!</b>\n\n{}", msg_str),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        Err(e) => {
            bot.send_message(
                msg.chat.id,
                format!("⚠️ <b>Error:</b> {}\n\nPlease check the number and try again, or /cancel.", e),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
    }

    Ok(())
}

/// Step 2: User enters the 6-digit OTP code.
async fn handle_otp_code(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    phone_number: String,
    account_service: Arc<AccountService>,
    scraping_service: Arc<ScrapingService>,
) -> HandlerResult {
    let otp_code = match msg.text() {
        Some(t) => t.trim(),
        None => {
            bot.send_message(msg.chat.id, "Please reply with the 6-digit code.")
                .await?;
            return Ok(());
        }
    };

    if otp_code.len() != 6 || !otp_code.chars().all(|c| c.is_ascii_digit()) {
        bot.send_message(
            msg.chat.id,
            "⚠️ The OTP code must be exactly 6 numbers (e.g. <code>123456</code>). Please try again:",
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
        return Ok(());
    }

    let _ = bot.send_message(msg.chat.id, "🔐 Verifying OTP and encrypting session at rest...").await;

    match account_service
        .verify_and_save_session(&phone_number, otp_code, "")
        .await
    {
        Ok(account) => {
            // Directly reset dialogue back to Start so user can immediately register the next phone!
            dialogue.update(State::Start).await?;

            bot.send_message(
                msg.chat.id,
                format!(
                    "✅ <b>Phone Verified & Saved!</b>\n\n\
                    Account <code>{}</code> is active, encrypted at rest, and enrolled in the 24/7 automated 12-hour vote schedule.\n\n\
                    ⚡ <b>Ready for next:</b> Paste another phone number directly to register it!",
                    account.phone_number
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;

            // Probe the website once with the newly issued token, even if local cooldown is stale.
            let scraping_ref = Arc::clone(&scraping_service);
            let account_id = account.id;
            let bot_for_probe = bot.clone();
            let chat_id = msg.chat.id;
            tokio::spawn(async move {
                match scraping_ref
                    .start_scraping_after_reauthentication(account_id)
                    .await
                {
                    Ok(record) => {
                        let message = if record.scraping_status
                            == crate::models::ScrapingStatus::AlreadyVoted
                        {
                            "Website response: this account already voted today; no new vote was added."
                                .to_string()
                        } else {
                            format!(
                                "Website probe completed: {} new vote records reported.",
                                record.records_scraped
                            )
                        };
                        let _ = bot_for_probe.send_message(chat_id, message).await;
                    }
                    Err(error) => {
                        let _ = bot_for_probe
                            .send_message(chat_id, format!("Website probe failed: {}", error))
                            .await;
                    }
                }
            });
        }
        Err(e) => {
            bot.send_message(
                msg.chat.id,
                format!("❌ <b>Verification Failed:</b> {}\n\nPlease re-enter the code, or /cancel.", e),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
    }

    Ok(())
}
