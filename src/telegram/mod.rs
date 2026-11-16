use crate::models::ScrapingRecord;
use crate::services::account_service::{ApplyResult, WorkerPaymentStats};
use crate::services::scraping_service::AccountVoteStatus;
use crate::services::{AccountService, ScrapingService};
use chrono::Utc;
use std::collections::BTreeMap;
use std::sync::Arc;
use teloxide::{
    dispatching::dialogue::InMemStorage,
    prelude::*,
    types::{InlineKeyboardButton, InlineKeyboardMarkup, MessageId},
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
    AwaitingApplicantName,
    AwaitingAccountSearch,
    AwaitingOtp {
        phone_number: String,
    },
    AwaitingCustomPaymentAmount {
        worker_telegram_id: i64,
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
    #[command(description = "Apply to become a worker and get bot access.")]
    Apply,
    #[command(description = "View pending worker applications (admins only).")]
    Request,
    #[command(description = "Become a worker and track your registered accounts.")]
    Worker,
    #[command(description = "Register a worker: /adminregister <telegram_id> <name>.")]
    Adminregister(String),
    #[command(description = "Show worker account counts.")]
    Admin,
    #[command(description = "Show worker payment tracker (admins only).")]
    Cash,
    #[command(description = "Record worker payment: /pay <list number> <amount> (admins only).")]
    Pay(String),
    #[command(description = "Worker control center: block or unblock workers (admins only).")]
    Control(String),
    #[command(description = "Show new and updated worker accounts.")]
    Check,
    #[command(description = "Show 20-minute operational activity report.")]
    Rep,
    #[command(description = "Show number of claimed and not claimed accounts.")]
    Claim,
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
    #[command(description = "Admin command reference (admins only).")]
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
                        .branch(dptree::case![State::AwaitingApplicantName].endpoint(handle_applicant_name))
                        .branch(dptree::case![State::AwaitingAccountSearch].endpoint(handle_account_search))
                        .branch(
                            dptree::case![State::AwaitingOtp { phone_number }]
                                .endpoint(handle_otp_code),
                        )
                        .branch(
                            dptree::case![State::AwaitingCustomPaymentAmount { worker_telegram_id }]
                                .endpoint(handle_custom_payment_amount),
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
    let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if !is_admin(&msg, &admin_telegram_ids) && account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        // Option B: Blocked worker ghost mode — completely ignore
        return Ok(());
    }

    match cmd {
        Command::Start => {
            dialogue.update(State::Start).await?;
            if is_admin(&msg, &admin_telegram_ids) {
                let welcome = "👋 <b>Welcome to the CreativeAwards Automated Voter Bot!</b>\n\n\
                    <b>Admin Commands:</b>\n\
                    /request — View & accept/reject worker applications\n\
                    /claim — Show daily claimed & error counts\n\
                    /cash — Worker payment tracker (cooldowns & payments)\n\
                    /pay <num> <amt> — Record payment shortcut (e.g. /pay 1 20)\n\
                    /control — Worker control center (block / unblock workers)\n\
                    /rep — Show 20-minute operational report\n\
                    /check — Show new and updated worker accounts\n\
                    /admin — Show worker account counts\n\
                    /status — Account status and ranges\n\
                    /register — Add a phone number & receive OTP\n\
                    /worker — Become a worker and track your accounts\n\
                    /cancel — Cancel current action\n\
                    /help — Show admin help reference";
                bot.send_message(msg.chat.id, welcome)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
            } else {
                let welcome = "👋 <b>Welcome to the CreativeAwards Voter Bot!</b>\n\n\
                    Paste any Ethiopian phone number (e.g. <code>0912345678</code> or <code>+251912345678</code>) directly to register it!\n\n\
                    <b>Commands:</b>\n\
                    /apply — Apply to become a worker and get bot access\n\
                    /register — Add a phone number & receive OTP\n\
                    /worker — Become a worker and track your accounts\n\
                    /cancel — Cancel current action";
                bot.send_message(msg.chat.id, welcome)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
            }
        }
        Command::Help => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(
                    msg.chat.id,
                    "⛔ This command is available to administrators only.",
                )
                .await?;
            } else {
                let help_text = "📖 <b>Administrator Commands:</b>\n\n\
                    📋 /request — Review & accept/reject worker applications\n\
                    ⚡ /claim — Show daily claim status (claimed, errors, waiting)\n\
                    💰 /cash — Worker payment tracker (view cooldowns, paid, remaining)\n\
                    💵 /pay <num> <amt> — Quick payment shortcut (e.g. /pay 1 20)\n\
                    🛡️ /control — Worker control center: block or unblock workers\n\
                    📊 /rep — Show 20-minute operational activity report\n\
                    🔄 /check — Show new and updated worker accounts\n\
                    👷 /admin — Show worker account counts and performance\n\
                    📱 /status — Real-time account status and cooldown browser\n\
                    🟢 /eligible — Accounts eligible to vote now\n\
                    ⏳ /cooldown — Accounts currently in cooldown\n\
                    🏥 /health — Bot and database health diagnostic\n\
                    ➕ /register — Manual phone registration with OTP\n\
                    🤝 /worker — Become a worker / view assigned accounts\n\
                    ❌ /cancel — Cancel current interactive action";
                bot.send_message(msg.chat.id, help_text)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
            }
        }
        Command::Status if !is_admin(&msg, &admin_telegram_ids) => {
            bot.send_message(msg.chat.id, "⛔ The /status command is available to administrators only.")
                .await?;
        }
        Command::Eligible | Command::Cooldown | Command::Health | Command::Rep | Command::Claim
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
        Command::Apply => {
            let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
            if let Ok(Some((name, status, is_blocked))) = account_service.get_worker_status_details(sender_id).await {
                if status == "APPROVED" && !is_blocked {
                    bot.send_message(
                        msg.chat.id,
                        format!("ℹ️ You are already registered and active as worker <b>{}</b>.", name),
                    )
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
                    return Ok(());
                } else if status == "PENDING" {
                    bot.send_message(
                        msg.chat.id,
                        "⏳ Your application is already submitted and pending review. Please wait for an administrator to accept or reject your request.",
                    )
                    .await?;
                    return Ok(());
                }
            }
            dialogue.update(State::AwaitingApplicantName).await?;
            bot.send_message(
                msg.chat.id,
                "👷 <b>Worker Application</b>\n\nPlease enter your full name to apply:",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        Command::Request => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to view worker requests.")
                    .await?;
            } else {
                send_pending_requests(&bot, msg.chat.id, &account_service, None).await?;
            }
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
                            let status_badge = if worker.is_blocked { "❌ " } else { "" };
                            let blocked_suffix = if worker.is_blocked { " [BLOCKED]" } else { "" };
                            format!(
                                "{}. {}<b>{}{}</b> — {} account(s), {} cooldown (<code>{}</code>)",
                                index + 1,
                                status_badge,
                                worker.display_name,
                                blocked_suffix,
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
        Command::Cash => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to view the payment tracker.")
                    .await?;
            } else {
                send_cash_tracker(&bot, msg.chat.id, &account_service, 0, None).await?;
            }
        }
        Command::Pay(args) => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to record worker payments.")
                    .await?;
            } else {
                handle_pay_command(&bot, msg.chat.id, &args, &account_service).await?;
            }
        }
        Command::Control(args) => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(msg.chat.id, "⛔ You are not authorized to access worker controls.")
                    .await?;
            } else {
                handle_control_command(&bot, msg.chat.id, &args, &account_service).await?;
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
        Command::Claim => {
            if !is_admin(&msg, &admin_telegram_ids) {
                bot.send_message(
                    msg.chat.id,
                    "⛔ This command is available to administrators only.",
                )
                .await?;
            } else {
                let summary = account_service.get_daily_claim_summary().await?;
                if summary.total_verified == 0 && summary.pending_otp_count == 0 {
                    bot.send_message(msg.chat.id, "ℹ️ No accounts registered yet.")
                        .await?;
                    return Ok(());
                }

                let total_verified = summary.total_verified;
                let claimed_pct = if total_verified > 0 {
                    (summary.claimed_count as f64 / total_verified as f64) * 100.0
                } else {
                    0.0
                };
                let error_pct = if total_verified > 0 {
                    (summary.total_error_count as f64 / total_verified as f64) * 100.0
                } else {
                    0.0
                };
                let not_claimed_pct = if total_verified > 0 {
                    (summary.not_claimed_count as f64 / total_verified as f64) * 100.0
                } else {
                    0.0
                };

                let mut response = format!(
                    "⚡ <b>Daily Claim Status</b>\n\n\
                    👥 <b>Total Verified Accounts:</b> {}\n\
                    ✅ <b>Successfully Claimed:</b> {} ({:.1}%)\n\
                    ❌ <b>Error:</b> {} ({:.1}%)\n\
                    ⏳ <b>Not Claimed:</b> {} ({:.1}%)\n",
                    total_verified,
                    summary.claimed_count,
                    claimed_pct,
                    summary.total_error_count,
                    error_pct,
                    summary.not_claimed_count,
                    not_claimed_pct
                );

                if summary.total_error_count > 0 {
                    response.push_str("\n<b>Error Breakdown:</b>\n");
                    if summary.already_voted_count > 0 {
                        response.push_str(&format!(
                            "   • Already Voted Today (HTTP 409): {}\n",
                            summary.already_voted_count
                        ));
                    }

                    if summary.failed_scrapes_count > 0 {
                        // Breakdown by category
                        let mut cat_counts = std::collections::HashMap::new();
                        for acc in &summary.failed_accounts {
                            let cat = simplify_error_category(&acc.error_message);
                            *cat_counts.entry(cat).or_insert(0usize) += 1;
                        }
                        let mut sorted_cats: Vec<_> = cat_counts.into_iter().collect();
                        sorted_cats.sort_by(|a, b| b.1.cmp(&a.1));
                        for (cat, count) in sorted_cats {
                            response.push_str(&format!("   • {}: {}\n", cat, count));
                        }

                        response.push_str("\n<b>Failed accounts (details):</b>\n");
                        let now = Utc::now();
                        for acc in summary.failed_accounts.iter().take(5) {
                            let label = acc.label.as_deref().unwrap_or("Account");
                            let snippet = format_error_snippet(&acc.error_message);
                            let retry_str = if let Some(retry_at) = acc.retry_after {
                                if retry_at > now {
                                    let mins = ((retry_at - now).num_seconds() + 59) / 60;
                                    format!("retry in {}m", mins)
                                } else {
                                    "ready to retry".to_string()
                                }
                            } else {
                                "retry pending".to_string()
                            };
                            response.push_str(&format!(
                                "• {} (#{}) — <code>{}</code>\n   └ <i>{}</i> ({})\n",
                                label, acc.account_id, acc.phone_number, snippet, retry_str
                            ));
                        }
                        if summary.failed_accounts.len() > 5 {
                            response.push_str(&format!(
                                "<i>...and {} more in 1-hr error retry.</i>\n",
                                summary.failed_accounts.len() - 5
                            ));
                        }
                    }
                }

                if summary.pending_otp_count > 0 {
                    response.push_str(&format!("\n📱 <b>Pending OTP Accounts:</b> {}\n", summary.pending_otp_count));
                }

                let unclaimed_preview = account_service.get_unclaimed_accounts(11).await?;
                if !unclaimed_preview.is_empty() && unclaimed_preview.len() <= 10 {
                    response.push_str("\n<b>Unclaimed accounts:</b>\n");
                    for acc in &unclaimed_preview {
                        let label = acc.label.as_deref().unwrap_or("Account");
                        response.push_str(&format!(
                            "• {} (#{}) — <code>{}</code>\n",
                            label, acc.id, acc.phone_number
                        ));
                    }
                } else if unclaimed_preview.len() > 10 {
                    response.push_str(&format!(
                        "\n<i>Use /eligible to view all {} unclaimed accounts.</i>\n",
                        summary.not_claimed_count
                    ));
                }

                response.push_str(&format!(
                    "\n🕒 <b>Daily Reset:</b> 00:00 UTC\n\
                     ⏰ <b>Daily Run:</b> Batches of 100 / 5 min from 05:00 UTC"
                ));

                bot.send_message(msg.chat.id, response)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .await?;
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
    let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        return Ok(());
    }

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
    let sender_id = query.from.id.0 as i64;
    if !admin_telegram_ids.contains(&sender_id) && account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        return Ok(());
    }

    if let Some(data) = query.data.clone() {
        if data.starts_with("ctrl_") {
            if !is_admin_callback(&query, &admin_telegram_ids) {
                bot.answer_callback_query(query.id)
                    .text("Admins only.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            }

            let chat_id = match query.message.as_ref() {
                Some(m) => m.chat().id,
                None => return Ok(()),
            };
            let msg_id = query.message.as_ref().map(|m| m.id());

            if let Some(page_str) = data.strip_prefix("ctrl_p:") {
                let page: usize = page_str.parse().unwrap_or(0);
                bot.answer_callback_query(query.id).await?;
                return send_control_center(&bot, chat_id, &account_service, page, msg_id).await;
            } else if let Some(id_str) = data.strip_prefix("ctrl_toggle:") {
                let parts: Vec<&str> = id_str.split(':').collect();
                if let Ok(worker_id) = parts[0].parse::<i64>() {
                    let page: usize = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(0);
                    match account_service.toggle_worker_blocked_status(worker_id).await {
                        Ok(Some((name, is_blocked))) => {
                            let text = if is_blocked {
                                format!("❌ Worker {} is now BLOCKED.", name)
                            } else {
                                format!("✅ Worker {} is now UNBLOCKED.", name)
                            };
                            let _ = bot.answer_callback_query(query.id).text(text).await;
                            return send_control_center(&bot, chat_id, &account_service, page, msg_id).await;
                        }
                        _ => {
                            let _ = bot.answer_callback_query(query.id).text("Worker not found.").show_alert(true).await;
                        }
                    }
                }
            }
            return Ok(());
        }

        if data.starts_with("req_") {
            if !is_admin_callback(&query, &admin_telegram_ids) {
                bot.answer_callback_query(query.id)
                    .text("Admins only.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            }

            let chat_id = match query.message.as_ref() {
                Some(m) => m.chat().id,
                None => return Ok(()),
            };
            let msg_id = query.message.as_ref().map(|m| m.id());

            if let Some(id_str) = data.strip_prefix("req_accept:") {
                if let Ok(worker_id) = id_str.parse::<i64>() {
                    if let Ok(Some(name)) = account_service.approve_worker_application(worker_id).await {
                        let text = format!("✅ Accepted {}! Worker is now active.", name);
                        let _ = bot.answer_callback_query(query.id).text(text).await;
                        let _ = bot.send_message(
                            ChatId(worker_id),
                            format!(
                                "🎉 <b>Congratulations, {}!</b>\n\n\
                                Your worker application has been <b>accepted</b>!\n\
                                You can now send /start and paste phone numbers to register them.",
                                name
                            ),
                        )
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await;
                        return send_pending_requests(&bot, chat_id, &account_service, msg_id).await;
                    }
                }
            } else if let Some(id_str) = data.strip_prefix("req_reject:") {
                if let Ok(worker_id) = id_str.parse::<i64>() {
                    if let Ok(Some(name)) = account_service.reject_worker_application(worker_id).await {
                        let text = format!("🚫 Rejected {}! Worker access denied.", name);
                        let _ = bot.answer_callback_query(query.id).text(text).await;
                        let _ = bot.send_message(
                            ChatId(worker_id),
                            "❌ <b>Application Update:</b>\n\nYour application to become a worker was not approved at this time.",
                        )
                        .parse_mode(teloxide::types::ParseMode::Html)
                        .await;
                        return send_pending_requests(&bot, chat_id, &account_service, msg_id).await;
                    }
                }
            }
            return Ok(());
        }

        if data.starts_with("cash_") {
            if !is_admin_callback(&query, &admin_telegram_ids) {
                bot.answer_callback_query(query.id)
                    .text("Admins only.")
                    .show_alert(true)
                    .await?;
                return Ok(());
            }
            bot.answer_callback_query(query.id).await?;

            let chat_id = match query.message.as_ref() {
                Some(m) => m.chat().id,
                None => return Ok(()),
            };
            let msg_id = query.message.as_ref().map(|m| m.id());

            if data == "cash_back" {
                return send_cash_tracker(&bot, chat_id, &account_service, 0, msg_id).await;
            } else if let Some(page_str) = data.strip_prefix("cash_p:") {
                let page: usize = page_str.parse().unwrap_or(0);
                return handle_cash_page_switch(&bot, chat_id, msg_id, page, &account_service).await;
            } else if let Some(id_str) = data.strip_prefix("cash_w:") {
                if let Ok(worker_id) = id_str.parse::<i64>() {
                    return handle_cash_worker_selection(&bot, chat_id, msg_id, worker_id, &account_service).await;
                }
            } else if let Some(rest) = data.strip_prefix("cash_pay:") {
                let parts: Vec<&str> = rest.split(':').collect();
                if parts.len() == 2 {
                    if let Ok(worker_id) = parts[0].parse::<i64>() {
                        return handle_cash_pay_quick(&bot, chat_id, msg_id, worker_id, parts[1], &account_service).await;
                    }
                }
            } else if let Some(id_str) = data.strip_prefix("cash_other:") {
                if let Ok(worker_id) = id_str.parse::<i64>() {
                    return handle_cash_other_prompt(&bot, chat_id, dialogue, worker_id, &account_service).await;
                }
            } else if let Some(id_str) = data.strip_prefix("cash_undo:") {
                if let Ok(worker_id) = id_str.parse::<i64>() {
                    return handle_cash_undo(&bot, chat_id, msg_id, worker_id, &account_service).await;
                }
            }
            return Ok(());
        }
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
    let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if let Ok(Some((_name, status, _))) = account_service.get_worker_status_details(sender_id).await {
        if status == "PENDING" {
            bot.send_message(
                msg.chat.id,
                "⏳ <b>Application Pending</b>\n\nYour worker application is currently pending review. Please wait until an administrator accepts your request before adding accounts.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    }
    if account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        return Ok(());
    }

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
    let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if let Ok(Some((_name, status, _))) = account_service.get_worker_status_details(sender_id).await {
        if status == "PENDING" {
            bot.send_message(
                msg.chat.id,
                "⏳ <b>Application Pending</b>\n\nYour worker application is currently pending review. Please wait until an administrator accepts your request before adding accounts.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    }
    if account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        return Ok(());
    }

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
    let sender_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(0);
    if account_service.is_worker_blocked(sender_id).await.unwrap_or(false) {
        return Ok(());
    }

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
                    Account <code>{}</code> is active.\n\n\
                    ⚡ <b>Ready for next:</b> Paste another phone number directly to register it!",
                    account.phone_number
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;

            // Probe the website once with the newly issued token in background without sending chat messages
            let scraping_ref = Arc::clone(&scraping_service);
            let account_id = account.id;
            tokio::spawn(async move {
                if let Err(error) = scraping_ref
                    .start_scraping_after_reauthentication(account_id)
                    .await
                {
                    tracing::warn!("Background vote probe failed for account #{}: {}", account_id, error);
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

fn simplify_error_category(err: &str) -> &'static str {
    let lower = err.to_lowercase();
    if lower.contains("network error") || lower.contains("error sending request") || lower.contains("connection") {
        "Network error / timeout"
    } else if lower.contains("429") || lower.contains("too many requests") {
        "Rate limit (HTTP 429)"
    } else if lower.contains("session") && (lower.contains("invalid") || lower.contains("missing") || lower.contains("expired")) {
        "Session invalid / expired"
    } else if lower.contains("status 5") || lower.contains("500") || lower.contains("502") || lower.contains("503") || lower.contains("504") {
        "Target server error (5xx)"
    } else if lower.contains("409") || lower.contains("already cast") || lower.contains("already voted") {
        "Already voted"
    } else {
        "Scraper error"
    }
}

fn format_error_snippet(err: &str) -> String {
    let lower = err.to_lowercase();
    if lower.contains("network error") || lower.contains("error sending request") {
        "Network connection error".to_string()
    } else if lower.contains("timed out") {
        "Request timed out".to_string()
    } else if lower.contains("session") && (lower.contains("invalid") || lower.contains("missing")) {
        "Auth session invalid/expired".to_string()
    } else if lower.contains("429") {
        "HTTP 429 Too Many Requests".to_string()
    } else if lower.contains("status 5") {
        "Target server 5xx error".to_string()
    } else {
        let clean = err.replace('\n', " ").replace('<', "&lt;").replace('>', "&gt;");
        if clean.len() > 40 {
            format!("{}...", &clean[..37])
        } else {
            clean
        }
    }
}

fn build_cash_keyboard(workers: &[WorkerPaymentStats], page: usize) -> InlineKeyboardMarkup {
    const PAGE_SIZE: usize = 20;
    let total_pages = (workers.len() + PAGE_SIZE - 1).max(1) / PAGE_SIZE.max(1);
    let total_pages = if total_pages == 0 { 1 } else { total_pages };
    let current_page = page.min(total_pages.saturating_sub(1));
    let start_idx = current_page * PAGE_SIZE;
    let end_idx = (start_idx + PAGE_SIZE).min(workers.len());

    let page_workers = &workers[start_idx..end_idx];
    let mut keyboard_rows = Vec::new();

    for chunk in page_workers.chunks(2) {
        let mut row = Vec::new();
        for w in chunk {
            let list_num = workers
                .iter()
                .position(|x| x.telegram_id == w.telegram_id)
                .unwrap_or(0)
                + 1;
            let badge = if w.is_blocked { "❌ " } else { "" };
            let btn_text = format!("{}{}. {} ({})", badge, list_num, w.display_name, w.remaining_cooldown);
            row.push(InlineKeyboardButton::callback(
                btn_text,
                format!("cash_w:{}", w.telegram_id),
            ));
        }
        keyboard_rows.push(row);
    }

    if total_pages > 1 {
        let mut nav_row = Vec::new();
        if current_page > 0 {
            nav_row.push(InlineKeyboardButton::callback(
                "◀️ Prev",
                format!("cash_p:{}", current_page - 1),
            ));
        }
        nav_row.push(InlineKeyboardButton::callback(
            format!("Page {}/{}", current_page + 1, total_pages),
            format!("cash_p:{}", current_page),
        ));
        if current_page + 1 < total_pages {
            nav_row.push(InlineKeyboardButton::callback(
                "Next ▶️",
                format!("cash_p:{}", current_page + 1),
            ));
        }
        keyboard_rows.push(nav_row);
    }

    InlineKeyboardMarkup::new(keyboard_rows)
}

async fn send_cash_tracker(
    bot: &Bot,
    chat_id: ChatId,
    account_service: &Arc<AccountService>,
    page: usize,
    edit_message_id: Option<MessageId>,
) -> HandlerResult {
    let workers = account_service.worker_payment_statistics().await?;
    if workers.is_empty() {
        if let Some(msg_id) = edit_message_id {
            bot.edit_message_text(chat_id, msg_id, "ℹ️ No workers are registered yet.")
                .await?;
        } else {
            bot.send_message(chat_id, "ℹ️ No workers are registered yet.")
                .await?;
        }
        return Ok(());
    }

    let total_cooldown: i64 = workers.iter().map(|w| w.cooldown_count).sum();
    let total_paid: i64 = workers.iter().map(|w| w.paid_cooldown).sum();
    let total_remaining: i64 = workers.iter().map(|w| w.remaining_cooldown).sum();

    let mut report = String::from("💰 <b>Worker Payment Tracker</b>\n\n");
    for (idx, worker) in workers.iter().enumerate() {
        let status_badge = if worker.is_blocked { "❌ " } else { "" };
        let blocked_suffix = if worker.is_blocked { " [BLOCKED]" } else { "" };
        report.push_str(&format!(
            "{}. {}{}{} - Cooldown: {} | Paid: {} | Remaining: {}\n",
            idx + 1,
            status_badge,
            worker.display_name,
            blocked_suffix,
            worker.cooldown_count,
            worker.paid_cooldown,
            worker.remaining_cooldown
        ));
    }
    report.push_str(&format!(
        "\n<b>Grand Total:</b>\n• Total Cooldown: {}\n• Total Paid: {}\n• Total Remaining: {}",
        total_cooldown, total_paid, total_remaining
    ));

    let keyboard = build_cash_keyboard(&workers, page);

    if let Some(msg_id) = edit_message_id {
        if report.len() <= 3800 {
            bot.edit_message_text(chat_id, msg_id, report)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
            return Ok(());
        } else {
            let _ = bot.delete_message(chat_id, msg_id).await;
        }
    }

    if report.len() <= 3800 {
        bot.send_message(chat_id, report)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    } else {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        for line in report.lines() {
            if current_chunk.len() + line.len() + 1 > 3800 {
                chunks.push(current_chunk);
                current_chunk = String::new();
            }
            if !current_chunk.is_empty() {
                current_chunk.push('\n');
            }
            current_chunk.push_str(line);
        }
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }

        let num_chunks = chunks.len();
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut msg_builder = bot
                .send_message(chat_id, chunk)
                .parse_mode(teloxide::types::ParseMode::Html);
            if i == num_chunks - 1 {
                msg_builder = msg_builder.reply_markup(keyboard.clone());
            }
            msg_builder.await?;
        }
    }

    Ok(())
}

async fn handle_cash_page_switch(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: Option<MessageId>,
    page: usize,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let workers = account_service.worker_payment_statistics().await?;
    let keyboard = build_cash_keyboard(&workers, page);
    if let Some(msg_id) = msg_id {
        let _ = bot
            .edit_message_reply_markup(chat_id, msg_id)
            .reply_markup(keyboard)
            .await;
    }
    Ok(())
}

async fn handle_cash_worker_selection(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: Option<MessageId>,
    telegram_id: i64,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let worker = match account_service.get_worker_payment_stats_by_id(telegram_id).await? {
        Some(w) => w,
        None => {
            bot.send_message(chat_id, "⚠️ Worker not found.").await?;
            return Ok(());
        }
    };

    let text = format!(
        "💵 <b>Pay Worker: {}</b>\n\n\
        • Cooldown: {}\n\
        • Paid: {}\n\
        • <b>Remaining: {}</b>\n\n\
        Select payment amount (in cooldowns):",
        worker.display_name,
        worker.cooldown_count,
        worker.paid_cooldown,
        worker.remaining_cooldown
    );

    let keyboard = InlineKeyboardMarkup::new(vec![
        vec![
            InlineKeyboardButton::callback("10", format!("cash_pay:{}:10", telegram_id)),
            InlineKeyboardButton::callback("20", format!("cash_pay:{}:20", telegram_id)),
            InlineKeyboardButton::callback("50", format!("cash_pay:{}:50", telegram_id)),
        ],
        vec![InlineKeyboardButton::callback(
            format!("Pay all remaining ({})", worker.remaining_cooldown),
            format!("cash_pay:{}:all", telegram_id),
        )],
        vec![
            InlineKeyboardButton::callback("Other", format!("cash_other:{}", telegram_id)),
            InlineKeyboardButton::callback("« Back to /cash", "cash_back".to_string()),
        ],
    ]);

    if let Some(msg_id) = msg_id {
        bot.edit_message_text(chat_id, msg_id, text)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    } else {
        bot.send_message(chat_id, text)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    }

    Ok(())
}

async fn handle_cash_pay_quick(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: Option<MessageId>,
    telegram_id: i64,
    amount_str: &str,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let worker = match account_service.get_worker_payment_stats_by_id(telegram_id).await? {
        Some(w) => w,
        None => {
            bot.send_message(chat_id, "⚠️ Worker not found.").await?;
            return Ok(());
        }
    };

    let amount = if amount_str == "all" {
        worker.remaining_cooldown
    } else {
        match amount_str.parse::<i64>() {
            Ok(v) => v,
            Err(_) => {
                bot.send_message(chat_id, "⚠️ Invalid payment amount.").await?;
                return Ok(());
            }
        }
    };

    if amount <= 0 {
        let msg = format!(
            "ℹ️ <b>{}</b> has no remaining cooldowns to pay (Remaining: 0).",
            worker.display_name
        );
        let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
            "« Back to /cash",
            "cash_back".to_string(),
        )]]);
        if let Some(msg_id) = msg_id {
            bot.edit_message_text(chat_id, msg_id, msg)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        } else {
            bot.send_message(chat_id, msg)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        return Ok(());
    }

    if amount > worker.remaining_cooldown {
        let msg = format!(
            "⚠️ Cannot pay {} cooldowns to <b>{}</b>. Remaining unpaid is only <b>{}</b>.",
            amount, worker.display_name, worker.remaining_cooldown
        );
        let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
            "« Back to /cash",
            "cash_back".to_string(),
        )]]);
        if let Some(msg_id) = msg_id {
            bot.edit_message_text(chat_id, msg_id, msg)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        } else {
            bot.send_message(chat_id, msg)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        return Ok(());
    }

    match account_service.record_worker_payment(telegram_id, amount).await {
        Ok(updated_worker) => {
            let confirmation = format!(
                "✅ <b>Payment Recorded!</b>\n\n\
                👷 <b>Worker:</b> {}\n\
                💵 <b>Paid Now:</b> {}\n\
                💰 <b>Total Paid:</b> {}\n\
                ⏳ <b>Remaining:</b> {}",
                updated_worker.display_name,
                amount,
                updated_worker.paid_cooldown,
                updated_worker.remaining_cooldown
            );
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback(
                    "↩️ Undo last payment",
                    format!("cash_undo:{}", telegram_id),
                )],
                vec![InlineKeyboardButton::callback("💵 Back to /cash", "cash_back".to_string())],
            ]);
            if let Some(msg_id) = msg_id {
                bot.edit_message_text(chat_id, msg_id, confirmation)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
            } else {
                bot.send_message(chat_id, confirmation)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
            }
        }
        Err(err) => {
            bot.send_message(chat_id, format!("❌ <b>Payment Error:</b> {}", err))
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
    }

    Ok(())
}

async fn handle_cash_other_prompt(
    bot: &Bot,
    chat_id: ChatId,
    dialogue: MyDialogue,
    telegram_id: i64,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let worker = match account_service.get_worker_payment_stats_by_id(telegram_id).await? {
        Some(w) => w,
        None => {
            bot.send_message(chat_id, "⚠️ Worker not found.").await?;
            return Ok(());
        }
    };

    dialogue
        .update(State::AwaitingCustomPaymentAmount {
            worker_telegram_id: telegram_id,
        })
        .await?;

    bot.send_message(
        chat_id,
        format!(
            "✍️ <b>Custom Payment for {}</b>\n\
            • Remaining unpaid: <b>{}</b> cooldowns\n\n\
            Please reply with the number of cooldowns to pay (e.g. <code>20</code>), or send /cancel:",
            worker.display_name, worker.remaining_cooldown
        ),
    )
    .parse_mode(teloxide::types::ParseMode::Html)
    .await?;

    Ok(())
}

async fn handle_custom_payment_amount(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    worker_telegram_id: i64,
    account_service: Arc<AccountService>,
    admin_telegram_ids: Vec<i64>,
) -> HandlerResult {
    if !is_admin(&msg, &admin_telegram_ids) {
        bot.send_message(msg.chat.id, "⛔ This action is available to administrators only.")
            .await?;
        return Ok(());
    }

    let raw_text = match msg.text() {
        Some(t) => t.trim(),
        None => {
            bot.send_message(msg.chat.id, "⚠️ Please enter a positive whole number.")
                .await?;
            return Ok(());
        }
    };

    let amount: i64 = match raw_text.parse() {
        Ok(num) if num > 0 => num,
        _ => {
            bot.send_message(
                msg.chat.id,
                "⚠️ Invalid amount. Please enter a positive whole number greater than 0 (e.g. <code>20</code>), or send /cancel.",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    };

    let worker = match account_service.get_worker_payment_stats_by_id(worker_telegram_id).await? {
        Some(w) => w,
        None => {
            dialogue.update(State::Start).await?;
            bot.send_message(msg.chat.id, "⚠️ Worker not found.").await?;
            return Ok(());
        }
    };

    if amount > worker.remaining_cooldown {
        bot.send_message(
            msg.chat.id,
            format!(
                "⚠️ Cannot pay {} cooldowns to <b>{}</b>. Remaining unpaid is only <b>{}</b>.\n\nPlease enter an amount up to {}, or send /cancel.",
                amount, worker.display_name, worker.remaining_cooldown, worker.remaining_cooldown
            ),
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
        return Ok(());
    }

    match account_service.record_worker_payment(worker_telegram_id, amount).await {
        Ok(updated_worker) => {
            dialogue.update(State::Start).await?;
            let confirmation = format!(
                "✅ <b>Payment Recorded!</b>\n\n\
                👷 <b>Worker:</b> {}\n\
                💵 <b>Paid Now:</b> {}\n\
                💰 <b>Total Paid:</b> {}\n\
                ⏳ <b>Remaining:</b> {}",
                updated_worker.display_name,
                amount,
                updated_worker.paid_cooldown,
                updated_worker.remaining_cooldown
            );
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback(
                    "↩️ Undo last payment",
                    format!("cash_undo:{}", worker_telegram_id),
                )],
                vec![InlineKeyboardButton::callback("💵 Back to /cash", "cash_back".to_string())],
            ]);
            bot.send_message(msg.chat.id, confirmation)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        Err(err) => {
            bot.send_message(msg.chat.id, format!("❌ <b>Payment Error:</b> {}", err))
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
    }

    Ok(())
}

async fn handle_cash_undo(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: Option<MessageId>,
    telegram_id: i64,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    match account_service.undo_worker_last_payment(telegram_id).await {
        Ok(Some((worker, reversed_amount))) => {
            let msg = format!(
                "↩️ <b>Payment Undone!</b>\n\n\
                👷 <b>Worker:</b> {}\n\
                ↩️ <b>Reversed Amount:</b> {}\n\
                💰 <b>Total Paid:</b> {}\n\
                ⏳ <b>Remaining:</b> {}",
                worker.display_name,
                reversed_amount,
                worker.paid_cooldown,
                worker.remaining_cooldown
            );
            let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                "💵 Back to /cash",
                "cash_back".to_string(),
            )]]);
            if let Some(msg_id) = msg_id {
                bot.edit_message_text(chat_id, msg_id, msg)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
            } else {
                bot.send_message(chat_id, msg)
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard)
                    .await?;
            }
        }
        Ok(None) => {
            let msg = "ℹ️ No recent payment found to undo for this worker.";
            let keyboard = InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
                "💵 Back to /cash",
                "cash_back".to_string(),
            )]]);
            if let Some(msg_id) = msg_id {
                bot.edit_message_text(chat_id, msg_id, msg)
                    .reply_markup(keyboard)
                    .await?;
            } else {
                bot.send_message(chat_id, msg)
                    .reply_markup(keyboard)
                    .await?;
            }
        }
        Err(err) => {
            bot.send_message(chat_id, format!("❌ <b>Undo Error:</b> {}", err))
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
    }

    Ok(())
}

async fn handle_pay_command(
    bot: &Bot,
    chat_id: ChatId,
    args: &str,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let parts: Vec<&str> = args.split_whitespace().collect();
    if parts.len() != 2 {
        bot.send_message(
            chat_id,
            "💡 <b>Usage:</b> <code>/pay &lt;list number&gt; &lt;amount&gt;</code>\n\
            Example: <code>/pay 1 20</code>\n\n\
            Use /cash to view the numbered list of workers.",
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
        return Ok(());
    }

    let list_number: usize = match parts[0].parse() {
        Ok(n) if n > 0 => n,
        _ => {
            bot.send_message(
                chat_id,
                "⚠️ The list number must be a positive integer greater than 0.",
            )
            .await?;
            return Ok(());
        }
    };

    let amount: i64 = match parts[1].parse() {
        Ok(n) if n > 0 => n,
        _ => {
            bot.send_message(
                chat_id,
                "⚠️ The payment amount must be a whole number greater than 0.",
            )
            .await?;
            return Ok(());
        }
    };

    let (worker, idx, _total) = match account_service.get_worker_by_list_number(list_number).await? {
        Some(item) => item,
        None => {
            let total = account_service.worker_payment_statistics().await?.len();
            bot.send_message(
                chat_id,
                format!(
                    "⚠️ Invalid worker number #{}. Please choose a number between 1 and {} from /cash.",
                    list_number, total
                ),
            )
            .await?;
            return Ok(());
        }
    };

    if amount > worker.remaining_cooldown {
        bot.send_message(
            chat_id,
            format!(
                "⚠️ Cannot pay {} cooldowns to <b>{}</b>. Remaining unpaid is only <b>{}</b>.",
                amount, worker.display_name, worker.remaining_cooldown
            ),
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .await?;
        return Ok(());
    }

    match account_service.record_worker_payment(worker.telegram_id, amount).await {
        Ok(updated_worker) => {
            let confirmation = format!(
                "✅ <b>Payment Recorded!</b>\n\n\
                👷 <b>Worker:</b> {} (#{}) (<code>{}</code>)\n\
                💵 <b>Paid Now:</b> {}\n\
                💰 <b>Total Paid:</b> {}\n\
                ⏳ <b>Remaining:</b> {}",
                updated_worker.display_name,
                idx,
                updated_worker.telegram_id,
                amount,
                updated_worker.paid_cooldown,
                updated_worker.remaining_cooldown
            );
            let keyboard = InlineKeyboardMarkup::new(vec![
                vec![InlineKeyboardButton::callback(
                    "↩️ Undo last payment",
                    format!("cash_undo:{}", worker.telegram_id),
                )],
                vec![InlineKeyboardButton::callback("💵 Back to /cash", "cash_back".to_string())],
            ]);
            bot.send_message(chat_id, confirmation)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
        }
        Err(err) => {
            bot.send_message(chat_id, format!("❌ <b>Payment Error:</b> {}", err))
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
    }

    Ok(())
}

fn build_control_keyboard(workers: &[WorkerPaymentStats], page: usize) -> InlineKeyboardMarkup {
    const PAGE_SIZE: usize = 10;
    let total_pages = (workers.len() + PAGE_SIZE - 1).max(1) / PAGE_SIZE.max(1);
    let total_pages = if total_pages == 0 { 1 } else { total_pages };
    let current_page = page.min(total_pages.saturating_sub(1));
    let start_idx = current_page * PAGE_SIZE;
    let end_idx = (start_idx + PAGE_SIZE).min(workers.len());

    let page_workers = &workers[start_idx..end_idx];
    let mut keyboard_rows = Vec::new();

    for chunk in page_workers.chunks(2) {
        let mut row = Vec::new();
        for w in chunk {
            let list_num = workers
                .iter()
                .position(|x| x.telegram_id == w.telegram_id)
                .unwrap_or(0)
                + 1;
            let btn_text = if w.is_blocked {
                format!("✅ Unblock {}. {}", list_num, w.display_name)
            } else {
                format!("🚫 Block {}. {}", list_num, w.display_name)
            };
            row.push(InlineKeyboardButton::callback(
                btn_text,
                format!("ctrl_toggle:{}:{}", w.telegram_id, current_page),
            ));
        }
        keyboard_rows.push(row);
    }

    if total_pages > 1 {
        let mut nav_row = Vec::new();
        if current_page > 0 {
            nav_row.push(InlineKeyboardButton::callback(
                "◀️ Prev",
                format!("ctrl_p:{}", current_page - 1),
            ));
        }
        nav_row.push(InlineKeyboardButton::callback(
            format!("Page {}/{}", current_page + 1, total_pages),
            format!("ctrl_p:{}", current_page),
        ));
        if current_page + 1 < total_pages {
            nav_row.push(InlineKeyboardButton::callback(
                "Next ▶️",
                format!("ctrl_p:{}", current_page + 1),
            ));
        }
        keyboard_rows.push(nav_row);
    }

    InlineKeyboardMarkup::new(keyboard_rows)
}

async fn send_control_center(
    bot: &Bot,
    chat_id: ChatId,
    account_service: &Arc<AccountService>,
    page: usize,
    edit_message_id: Option<MessageId>,
) -> HandlerResult {
    let workers = account_service.worker_payment_statistics().await?;
    if workers.is_empty() {
        if let Some(msg_id) = edit_message_id {
            bot.edit_message_text(chat_id, msg_id, "ℹ️ No workers are registered yet.")
                .await?;
        } else {
            bot.send_message(chat_id, "ℹ️ No workers are registered yet.")
                .await?;
        }
        return Ok(());
    }

    let blocked_count = workers.iter().filter(|w| w.is_blocked).count();
    let active_count = workers.len() - blocked_count;

    let mut report = format!(
        "🛡️ <b>Worker Control Center</b>\n\n\
        Workers: {} total (🟢 {} active, ❌ {} blocked)\n\n\
        <i>Blocked workers cannot register new phone numbers and the bot ignores them. All their existing accounts stay active and voting 24/7.</i>\n\n",
        workers.len(),
        active_count,
        blocked_count
    );

    for (idx, worker) in workers.iter().enumerate() {
        let status_icon = if worker.is_blocked { "❌" } else { "🟢" };
        let status_label = if worker.is_blocked { "BLOCKED" } else { "Active" };
        report.push_str(&format!(
            "{}. {} <b>{}</b> [{}] — {} account(s) (<code>{}</code>)\n",
            idx + 1,
            status_icon,
            worker.display_name,
            status_label,
            worker.account_count,
            worker.telegram_id
        ));
    }

    report.push_str("\n💡 <i>Tap a button below or type <code>/control &lt;number&gt;</code> to block/unblock.</i>");

    let keyboard = build_control_keyboard(&workers, page);

    if let Some(msg_id) = edit_message_id {
        if report.len() <= 3800 {
            bot.edit_message_text(chat_id, msg_id, report)
                .parse_mode(teloxide::types::ParseMode::Html)
                .reply_markup(keyboard)
                .await?;
            return Ok(());
        } else {
            let _ = bot.delete_message(chat_id, msg_id).await;
        }
    }

    if report.len() <= 3800 {
        bot.send_message(chat_id, report)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    } else {
        let mut chunks = Vec::new();
        let mut current_chunk = String::new();
        for line in report.lines() {
            if current_chunk.len() + line.len() + 1 > 3800 {
                chunks.push(current_chunk);
                current_chunk = String::new();
            }
            if !current_chunk.is_empty() {
                current_chunk.push('\n');
            }
            current_chunk.push_str(line);
        }
        if !current_chunk.is_empty() {
            chunks.push(current_chunk);
        }
        for (i, chunk) in chunks.iter().enumerate() {
            let mut send = bot.send_message(chat_id, chunk).parse_mode(teloxide::types::ParseMode::Html);
            if i == chunks.len() - 1 {
                send = send.reply_markup(keyboard.clone());
            }
            send.await?;
        }
    }

    Ok(())
}

async fn handle_control_command(
    bot: &Bot,
    chat_id: ChatId,
    args: &str,
    account_service: &Arc<AccountService>,
) -> HandlerResult {
    let trimmed = args.trim();
    if trimmed.is_empty() {
        return send_control_center(bot, chat_id, account_service, 0, None).await;
    }

    let parts: Vec<&str> = trimmed.split_whitespace().collect();
    let (action, list_str) = match parts.len() {
        1 => (None, parts[0]),
        2 => (Some(parts[0].to_lowercase()), parts[1]),
        _ => {
            bot.send_message(
                chat_id,
                "⚠️ Usage: <code>/control</code> or <code>/control &lt;number&gt;</code> (e.g. <code>/control 2</code>).",
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    };

    let list_number = match list_str.parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => {
            bot.send_message(
                chat_id,
                format!("⚠️ Invalid worker number '{}'. Please provide a number from <code>/control</code>.", list_str),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
            return Ok(());
        }
    };

    let (worker, idx, _total) = match account_service.get_worker_by_list_number(list_number).await? {
        Some(item) => item,
        None => {
            let total = account_service.worker_payment_statistics().await?.len();
            bot.send_message(
                chat_id,
                format!("⚠️ Invalid worker number #{}. Please choose a number between 1 and {} from /control.", list_number, total),
            )
            .await?;
            return Ok(());
        }
    };

    let (name, is_blocked) = match action.as_deref() {
        Some("block") => {
            account_service.set_worker_blocked_status(worker.telegram_id, true).await?;
            (worker.display_name.clone(), true)
        }
        Some("unblock") => {
            account_service.set_worker_blocked_status(worker.telegram_id, false).await?;
            (worker.display_name.clone(), false)
        }
        _ => {
            account_service.toggle_worker_blocked_status(worker.telegram_id).await?
                .unwrap_or((worker.display_name.clone(), !worker.is_blocked))
        }
    };

    let page = idx.saturating_sub(1) / 10;

    if is_blocked {
        bot.send_message(
            chat_id,
            format!(
                "❌ <b>Worker Blocked!</b>\n\n\
                👷 <b>Worker:</b> {} (#{})\n\
                🆔 <b>Telegram ID:</b> <code>{}</code>\n\
                📊 <b>Accounts in database:</b> {}\n\n\
                • This worker is now blocked and ignored by the bot.\n\
                • They cannot register or add any new phone numbers.\n\
                • All their {} existing accounts remain safe and active in the 24/7 vote schedule.",
                name, idx, worker.telegram_id, worker.account_count, worker.account_count
            ),
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .reply_markup(InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback(format!("✅ Unblock {}. {}", idx, name), format!("ctrl_toggle:{}", worker.telegram_id)),
            InlineKeyboardButton::callback("🛡️ View /control", "ctrl_p:0".to_string()),
            InlineKeyboardButton::callback(format!("✅ Unblock {}. {}", idx, name), format!("ctrl_toggle:{}:{}", worker.telegram_id, page)),
            InlineKeyboardButton::callback("🛡️ View /control", format!("ctrl_p:{}", page)),
        ]]))
        .await?;
    } else {
        bot.send_message(
            chat_id,
            format!(
                "✅ <b>Worker Unblocked!</b>\n\n\
                👷 <b>Worker:</b> {} (#{})\n\
                🆔 <b>Telegram ID:</b> <code>{}</code>\n\n\
                • This worker has been restored to active status.\n\
                • They can now use the bot normally.",
                name, idx, worker.telegram_id
            ),
        )
        .parse_mode(teloxide::types::ParseMode::Html)
        .reply_markup(InlineKeyboardMarkup::new(vec![vec![
            InlineKeyboardButton::callback(format!("🚫 Block {}. {}", idx, name), format!("ctrl_toggle:{}:{}", worker.telegram_id, page)),
            InlineKeyboardButton::callback("🛡️ View /control", format!("ctrl_p:{}", page)),
        ]]))
        .await?;
    }

    Ok(())
}

async fn handle_applicant_name(
    bot: Bot,
    dialogue: MyDialogue,
    msg: Message,
    account_service: Arc<AccountService>,
    admin_telegram_ids: Vec<i64>,
) -> HandlerResult {
    let Some(name) = msg.text().map(str::trim).filter(|name| !name.is_empty()) else {
        bot.send_message(msg.chat.id, "Please enter a valid name, or use /cancel.")
            .await?;
        return Ok(());
    };

    let Some(user) = msg.from.as_ref() else {
        bot.send_message(msg.chat.id, "Could not identify your Telegram account. Please try again.")
            .await?;
        return Ok(());
    };

    let res = account_service.apply_as_worker(user.id.0 as i64, name).await?;
    dialogue.update(State::Start).await?;

    match res {
        ApplyResult::Submitted => {
            bot.send_message(
                msg.chat.id,
                format!(
                    "⏳ <b>Application Submitted!</b>\n\n\
                    Thank you, <b>{}</b>. Your request has been sent to the administrators.\n\
                    Please wait until an administrator accepts or rejects your application.",
                    name
                ),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;

            // Notify all admins, especially primary admin 5924416179
            let notify_text = format!(
                "🔔 <b>New Worker Application Received!</b>\n\n\
                👷 <b>Name:</b> {}\n\
                🆔 <b>Telegram ID:</b> <code>{}</code>\n\
                👤 <b>Username:</b> @{}\n\n\
                Choose an action below or send /request:",
                name,
                user.id.0,
                user.username.as_deref().unwrap_or("none")
            );
            let keyboard = InlineKeyboardMarkup::new(vec![vec![
                InlineKeyboardButton::callback("✅ Accept", format!("req_accept:{}", user.id.0)),
                InlineKeyboardButton::callback("🚫 Reject", format!("req_reject:{}", user.id.0)),
            ]]);

            for admin_id in &admin_telegram_ids {
                let _ = bot.send_message(ChatId(*admin_id), notify_text.clone())
                    .parse_mode(teloxide::types::ParseMode::Html)
                    .reply_markup(keyboard.clone())
                    .await;
            }
        }
        ApplyResult::AlreadyActive(name) => {
            bot.send_message(
                msg.chat.id,
                format!("ℹ️ You are already registered and active as worker <b>{}</b>.", name),
            )
            .parse_mode(teloxide::types::ParseMode::Html)
            .await?;
        }
        ApplyResult::AlreadyPending => {
            bot.send_message(
                msg.chat.id,
                "⏳ Your application is already submitted and waiting for review. Please wait until an administrator accepts or rejects your application.",
            )
            .await?;
        }
    }

    Ok(())
}

async fn send_pending_requests(
    bot: &Bot,
    chat_id: ChatId,
    account_service: &Arc<AccountService>,
    edit_message_id: Option<MessageId>,
) -> HandlerResult {
    let pending = account_service.list_pending_worker_applications().await?;
    if pending.is_empty() {
        let text = "ℹ️ <b>No pending worker applications.</b>\n\nAll applications have been reviewed.";
        if let Some(msg_id) = edit_message_id {
            bot.edit_message_text(chat_id, msg_id, text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        } else {
            bot.send_message(chat_id, text)
                .parse_mode(teloxide::types::ParseMode::Html)
                .await?;
        }
        return Ok(());
    }

    let mut report = format!(
        "📋 <b>Worker Applications Pending Approval</b>\n\n\
        Pending requests: <b>{}</b>\n\n",
        pending.len()
    );

    let mut keyboard_rows = Vec::new();
    for (idx, app) in pending.iter().enumerate() {
        report.push_str(&format!(
            "{}. <b>{}</b> (<code>{}</code>)\n",
            idx + 1,
            app.display_name,
            app.telegram_id
        ));

        keyboard_rows.push(vec![
            InlineKeyboardButton::callback(
                format!("✅ Accept {}. {}", idx + 1, app.display_name),
                format!("req_accept:{}", app.telegram_id),
            ),
            InlineKeyboardButton::callback(
                format!("🚫 Reject {}. {}", idx + 1, app.display_name),
                format!("req_reject:{}", app.telegram_id),
            ),
        ]);
    }

    let keyboard = InlineKeyboardMarkup::new(keyboard_rows);

    if let Some(msg_id) = edit_message_id {
        bot.edit_message_text(chat_id, msg_id, report)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    } else {
        bot.send_message(chat_id, report)
            .parse_mode(teloxide::types::ParseMode::Html)
            .reply_markup(keyboard)
            .await?;
    }

    Ok(())
}
