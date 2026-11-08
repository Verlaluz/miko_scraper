use crate::services::{AccountService, ScrapingService};
use std::sync::Arc;
use teloxide::{
    dispatching::dialogue::InMemStorage,
    prelude::*,
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
    #[command(description = "View real-time voting eligibility for all accounts.")]
    Status,
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
) {
    tokio::spawn(async move {
        let storage = InMemStorage::<State>::new();
        loop {
            tracing::info!("Initializing Telegram Bot dispatcher...");
            let bot = Bot::new(token.clone());

            let handler = Update::filter_message()
                .enter_dialogue::<Message, InMemStorage<State>, State>()
                .branch(
                    dptree::entry()
                        .filter_command::<Command>()
                        .endpoint(handle_command),
                )
                .branch(dptree::case![State::Start].endpoint(handle_start_text))
                .branch(dptree::case![State::AwaitingPhoneNumber].endpoint(handle_phone_number))
                .branch(
                    dptree::case![State::AwaitingOtp { phone_number }]
                        .endpoint(handle_otp_code),
                );

            let mut dispatcher = Dispatcher::builder(bot, handler)
                .dependencies(dptree::deps![
                    storage.clone(),
                    Arc::clone(&account_service),
                    Arc::clone(&scraping_service)
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
) -> HandlerResult {
    match cmd {
        Command::Start | Command::Help => {
            dialogue.update(State::Start).await?;
            let welcome = "👋 <b>Welcome to the CreativeAwards Automated Voter Bot!</b>\n\n\
                Use this bot to register phone accounts, verify OTPs, and track 12-hour votes for family & friends.\n\n\
                <b>Commands:</b>\n\
                /register — Add a phone number & receive OTP\n\
                /status — View eligibility of all registered accounts\n\
                /cancel — Cancel current action\n\
                /help — Show this help message";
            bot.send_message(msg.chat.id, welcome)
                .parse_mode(teloxide::types::ParseMode::Html)
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
        Command::Status => {
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
            let mut response = String::from("📊 <b>Family & Friends Voting Status:</b>\n\n");

            for s in statuses {
                let name = s.label.unwrap_or_else(|| format!("Account #{}", s.account_id));
                let status_icon = if s.is_eligible {
                    "🟢 <b>Ready to Vote</b>"
                } else {
                    let hours = s.remaining_seconds / 3600;
                    let mins = (s.remaining_seconds % 3600) / 60;
                    format!("🟡 <b>Cooldown ({}h {}m left)</b>", hours, mins).leak()
                };

                response.push_str(&format!(
                    "👤 <b>{}</b> (<code>{}</code>)\nStatus: {}\n\n",
                    name, s.phone_number, status_icon
                ));
            }

            bot.send_message(msg.chat.id, response)
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
        "💡 <b>Tip:</b> Paste any Ethiopian phone number (e.g. <code>0912345678</code> or <code>+251912345678</code>) directly to register it!\n\nOr use /status to view voting eligibility timers.",
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
                format!("ℹ️ <b>Already Registered!</b>\n\n{}\n\nUse /status to see its current voting countdown.", msg_str),
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

            // Trigger an initial vote claim or status sync asynchronously so Telegram doesn't block
            let scraping_ref = Arc::clone(&scraping_service);
            let account_id = account.id;
            tokio::spawn(async move {
                let _ = scraping_ref.start_scraping(account_id).await;
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
