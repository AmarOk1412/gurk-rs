use crate::appdata::AppData;
use crate::util::*;

use app_dirs::{get_app_dir, AppDataType, AppInfo};
use chrono::{TimeZone, Utc};
use crossterm::event::KeyCode;
use dirs;
use jami_rs::{ImportType, Jami};
use jami_rs::account::Account;
use unicode_width::UnicodeWidthStr;

use std::collections::HashMap;
use std::fs::{copy, create_dir, File};
use std::io::Write;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub struct App {
    pub should_quit: bool,
    pub log_file: Option<File>,
    pub data: AppData,
}

impl App {
    /**
     * Create new app
     */
    pub fn try_new(verbose: bool) -> anyhow::Result<Self> {
        let log_file = if verbose {
            Some(File::create("jami-cli.log").unwrap())
        } else {
            None
        };
        let mut data = AppData::init_from_jami()?;
        data.lookup_members();
        if data.channels.state.selected().is_none() && !data.channels.items.is_empty() {
            data.channels.state.select(Some(0));
        }

        Ok(Self {
            data,
            should_quit: false,
            log_file,
        })
    }

    /**
     * Handle key events
     * @param self
     * @param key   key code
     */
    pub fn on_key(&mut self, key: KeyCode) {
        match key {
            KeyCode::Char(c) => {
                let idx = self
                    .data
                    .input
                    .chars()
                    .take(self.data.input_cursor)
                    .map(|c| c.len_utf8())
                    .sum();
                self.data.input.insert(idx, c);
                self.data.input_cursor += 1;
            }
            KeyCode::Enter if !self.data.input.is_empty() => {
                if let Some(idx) = self.data.channels.state.selected() {
                    self.send_input(idx)
                }
            }
            KeyCode::Backspace => {
                if self.data.input_cursor > 0
                    && self.data.input_cursor < self.data.input.width() + 1
                {
                    self.data.input_cursor = self.data.input_cursor.saturating_sub(1);
                    let idx = self
                        .data
                        .input
                        .chars()
                        .take(self.data.input_cursor)
                        .map(|c| c.len_utf8())
                        .sum();
                    self.data.input.remove(idx);
                }
            }
            _ => {}
        }
    }

    /**
     * Handle user input
     * @param self
     * @param channel_idx       The channel which receives the input
     */
    fn send_input(&mut self, channel_idx: usize) {
        let channel = &mut self.data.channels.items[channel_idx];

        let message: String = self.data.input.drain(..).collect();
        self.data.input_cursor = 0;

        if message == "/exit" {
            self.should_quit = true;
            return;
        }

        let mut show_msg = true;
        let mut is_invite = false;
        let mut is_trust_request = false;
        let mut from_request = String::new();
        match channel.channel_type.clone() {
            ChannelType::Invite => {
                is_invite = true;
            }
            ChannelType::TrustRequest(contact) => {
                is_trust_request = true;
                from_request = contact;
            }
            _ => {}
        }

        if message.starts_with("/msg ") {
            let account_id = &self.data.account.id;
            let mut member = String::from(message.strip_prefix("/msg ").unwrap());
            if Jami::is_hash(&member) {
                Jami::add_contact(&account_id, &member);
                Jami::send_trust_request(&account_id, &member, Vec::new() /* TODO */);
            } else {
                let mut ns = String::new();
                if member.find("@") != None {
                    let member_cloned = member.clone();
                    let split: Vec<&str> = member_cloned.split("@").collect();
                    member = split[0].to_string();
                    ns = split[1].to_string();
                }
                self.data.out_invite.push(OutgoingInvite {
                    account: account_id.to_string(),
                    channel: None,
                    member: member.clone(),
                });
                show_msg = false;
                Jami::lookup_name(&account_id, &ns, &member);
            }
        } else if channel.channel_type == ChannelType::Generated {
            if message == "/new" {
                Jami::start_conversation(&self.data.account.id);
            } else if message == "/list" {
                for account in Jami::get_account_list() {
                    channel
                        .messages
                        .push(Message::info(String::from(format!("{}", account))));
                }
            } else if message == "/get" || message.starts_with("/get ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let filter = parts.get(1).unwrap_or(&"").to_string();
                for (key, value) in Jami::get_account_details(&self.data.account.id) {
                    if filter.is_empty() || filter.to_lowercase() == key.to_lowercase() {
                        channel
                            .messages
                            .push(Message::info(String::from(format!("{}: {}", key, value))));
                    }
                }
                show_msg = false;
            } else if message.starts_with("/set") {
                let parts: Vec<&str> = message.split(" ").collect();
                let key = parts.get(1).unwrap_or(&"").to_string();
                let value = parts.get(2).unwrap_or(&"").to_string();
                let mut details = Jami::get_account_details(&self.data.account.id);
                let mut key_found = String::new();
                for (key2, _) in &details {
                    if key2.to_lowercase() == key.to_lowercase() {
                        key_found = key2.to_string();
                    }
                }
                if !key_found.is_empty() {
                    details.insert(key_found, value.to_string());
                }
                Jami::set_account_details(&self.data.account.id, details);
                show_msg = false;
            } else if message.starts_with("/switch ") {
                let account_id = String::from(message.strip_prefix("/switch ").unwrap());
                let account = Jami::get_account(&*account_id);
                if account.id.is_empty() {
                    channel
                        .messages
                        .push(Message::info(String::from("Invalid account id.")));
                } else {
                    //  TODO avoid duplicate code
                    self.untrack_current_conversation();
                    self.data.account = account;
                    self.data
                        .profile_manager
                        .load_from_account(&self.data.account.id);
                    let channels = AppData::channels_for_account(&self.data.account);
                    self.data.channels = StatefulList::with_items(channels);
                    if !self.data.channels.items.is_empty() {
                        self.data.channels.state.select(Some(0));
                    }
                    self.data.lookup_members();
                }
            } else if message == "/add" {
                Jami::add_account("", "", ImportType::None);
            } else if message.starts_with("/rm ") {
                let account_id = String::from(message.strip_prefix("/rm ").unwrap());
                Jami::rm_account(&*account_id);
            } else if message.starts_with("/import ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let file = parts.get(1).unwrap_or(&"").to_string();
                let password = parts.get(2).unwrap_or(&"").to_string();
                Jami::add_account(&file, &password, ImportType::BACKUP);
            } else if message.starts_with("/link ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let pin = parts.get(1).unwrap_or(&"").to_string();
                let password = parts.get(2).unwrap_or(&"").to_string();
                Jami::add_account(&pin, &password, ImportType::NETWORK);
            } else if message == "/help" {
                channel
                    .messages
                    .push(Message::info(String::from("/help: Show this help")));
                channel.messages.push(Message::info(String::from(
                    "/new: Start a new conversation",
                )));
                channel.messages.push(Message::info(String::from(
                    "/msg <id|username>: Start a conversation with someone",
                )));
                channel
                    .messages
                    .push(Message::info(String::from("/list: list accounts")));
                channel.messages.push(Message::info(String::from(
                    "/switch <id>: switch to an account",
                )));
                channel
                    .messages
                    .push(Message::info(String::from("/add: Add a new account")));
                channel
                    .messages
                    .push(Message::info(String::from("/rm <id>: Remove an account")));
                channel.messages.push(Message::info(String::from(
                    "/link <pin> [password]: Link an account via a PIN",
                )));
                channel.messages.push(Message::info(String::from(
                    "/import <file> [password]: Import an account from a backup",
                )));
                channel.messages.push(Message::info(String::from(
                    "/get [key]: get account details (if key specified, only get key)",
                )));
                channel.messages.push(Message::info(String::from(
                    "/set <key> <value>: set account detail",
                )));
                channel
                    .messages
                    .push(Message::info(String::from("/exit: quit")));
            }
        } else if channel.channel_type == ChannelType::Group {
            let account_id = &self.data.account.id;
            if message == "/leave" {
                if Jami::rm_conversation(&account_id, &channel.id) {
                    return;
                } else {
                    channel
                        .messages
                        .push(Message::info(String::from("Cannot remove conversation")));
                }
            } else if message.starts_with("/invite") {
                let mut member = String::from(message.strip_prefix("/invite ").unwrap());
                if Jami::is_hash(&member) {
                    Jami::add_conversation_member(&account_id, &channel.id, &member);
                } else {
                    let mut ns = String::new();
                    if member.find("@") != None {
                        let member_cloned = member.clone();
                        let split: Vec<&str> = member_cloned.split("@").collect();
                        member = split[0].to_string();
                        ns = split[1].to_string();
                    }
                    self.data.out_invite.push(OutgoingInvite {
                        account: account_id.to_string(),
                        channel: Some(channel.id.clone()),
                        member: member.clone(),
                    });
                    show_msg = false;
                    Jami::lookup_name(&account_id, &ns, &member);
                }
            } else if message.starts_with("/title") {
                let title = String::from(message.strip_prefix("/title ").unwrap());
                let mut infos = HashMap::new();
                infos.insert(String::from("title"), title);
                Jami::update_conversation_infos(&account_id, &channel.id, infos);
                show_msg = false;
            } else if message.starts_with("/description") {
                let description = String::from(message.strip_prefix("/description ").unwrap());
                let mut infos = HashMap::new();
                infos.insert(String::from("description"), description);
                Jami::update_conversation_infos(&account_id, &channel.id, infos);
                show_msg = false;
            } else if message.starts_with("/kick") {
                let mut member = String::from(message.strip_prefix("/kick ").unwrap());
                if Jami::is_hash(&member) {
                    Jami::rm_conversation_member(&account_id, &channel.id, &member);
                } else {
                    let mut ns = String::new();
                    if member.find("@") != None {
                        let member_cloned = member.clone();
                        let split: Vec<&str> = member_cloned.split("@").collect();
                        member = split[0].to_string();
                        ns = split[1].to_string();
                    }
                    self.data.pending_rm.push(PendingRm {
                        account: account_id.to_string(),
                        channel: channel.id.clone(),
                        member: member.clone(),
                    });
                    show_msg = false;
                    Jami::lookup_name(&account_id, &ns, &member);
                }
            } else if message.starts_with("/send ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let path = parts.get(1).unwrap_or(&"").to_string();
                Jami::send_file(&account_id.to_string(), &channel.id.clone(), &path, &path, &String::new());
                show_msg = false;
            } else if message.starts_with("/accept ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let tid = parts.get(1).unwrap_or(&"").to_string().parse::<u64>().unwrap_or(0);
                let mut path = parts.get(2).unwrap_or(&"").to_string();
                if path.is_empty() {
                    let default_download_dir = format!("{}/Jami", dirs::download_dir().unwrap().into_os_string().into_string().unwrap());
                    let _ = create_dir(default_download_dir.clone());
                    let info = Jami::data_transfer_info(account_id.clone(), channel.id.clone(), tid);
                    if info.is_none() {
                        channel.messages.push(Message::info(String::from(
                            "Cannot accept file",
                        )));
                    } else {
                        let info = info.unwrap();
                        let mut idx = 0;
                        loop {
                            let p = match idx {
                                0 => format!("{}/{}", default_download_dir, info.display_name.clone()),
                                i => format!("{}/{}_{}", default_download_dir, info.display_name.clone(), i),
                            };
                            if !Path::new(&p).exists() {
                                path = p.clone();
                                break;
                            }
                            idx += 1;
                        }
                    }
                }
                if !path.is_empty() {
                    Jami::accept_file_transfer(&account_id, &channel.id, tid, &path);
                }
                show_msg = false;
            } else if message.starts_with("/cancel ") {
                let parts: Vec<&str> = message.split(" ").collect();
                let tid = parts.get(1).unwrap_or(&"").to_string().parse::<u64>().unwrap_or(0);
                Jami::cancel_file_transfer(&account_id, &channel.id, tid);
                show_msg = false;
            } else if message == "/help" {
                channel
                    .messages
                    .push(Message::info(String::from("/help: Show this help")));
                channel.messages.push(Message::info(String::from(
                    "/leave: Leave this conversation",
                )));
                channel.messages.push(Message::info(String::from(
                    "/invite [hash|username]: Invite somebody to the conversation",
                )));
                channel.messages.push(Message::info(String::from(
                    "/kick [hash|username]: Kick someone from the conversation",
                )));
                channel.messages.push(Message::info(String::from(
                    "/title [title]: Change the title of the room",
                )));
                channel.messages.push(Message::info(String::from(
                    "/description [description]: Change the description of the room",
                )));
                channel.messages.push(Message::info(String::from(
                    "/send [path]: Send a file to the conversation",
                )));
                channel.messages.push(Message::info(String::from(
                    "/accept [tid] <path>: Accept a file transfer",
                )));
                channel.messages.push(Message::info(String::from(
                    "/cancel [tid]: Cancel a file transfer",
                )));
                channel
                    .messages
                    .push(Message::info(String::from("/exit: quit")));
            } else {
                show_msg = false;
                let flag = 0; /* not a reply, not an edit, just a single msg */
                Jami::send_message(&account_id, &channel.id, &message, &String::new(), &flag);
            }
        } else if is_invite || is_trust_request {
            let account_id = &self.data.account.id;
            if message == "/leave" {
                if is_invite {
                    Jami::decline_request(&account_id, &channel.id);
                } else {
                    Jami::discard_trust_request(&account_id, &from_request);
                }
                self.data.channels.items.remove(channel_idx);
                if !self.data.channels.items.is_empty() {
                    self.data.channels.state.select(Some(0));
                }
                show_msg = false;
            } else if message == "/join" {
                if is_invite {
                    Jami::accept_request(&account_id, &channel.id);
                    channel
                        .messages
                        .push(Message::info(String::from("Syncing… the view will update")));
                } else {
                    Jami::accept_trust_request(&account_id, &from_request);
                    if !self.data.channels.items.is_empty() {
                        self.data.channels.state.select(Some(0));
                    }
                    self.data.channels.items.remove(channel_idx);
                    show_msg = false;
                }
            } else {
                channel
                    .messages
                    .push(Message::info(String::from("/help: Show this help")));
                channel
                    .messages
                    .push(Message::info(String::from("/leave: Decline this request")));
                channel
                    .messages
                    .push(Message::info(String::from("/join: Accepts the request")));
            }
        }

        if show_msg {
            let channel = &mut self.data.channels.items[channel_idx];
            channel.messages.push(Message::new(
                self.data.account.get_display_name(),
                message.clone(),
                Utc::now(),
            ));
        }

        self.reset_unread_messages();
        self.bubble_up_channel(channel_idx);
    }

    /**
     * Handle incoming messages
     * @param self
     * @param account_id
     * @param conversation_id
     * @param payloads
     * @return if ok
     */
    pub async fn on_message(
        &mut self,
        account_id: &String,
        conversation_id: &String,
        payloads: HashMap<String, String>,
    ) -> Option<()> {
        self.log(format!("incoming: {:?}", payloads));
        if account_id == &*self.data.account.id {
            for channel in &mut *self.data.channels.items {
                if &*channel.id == conversation_id {
                    // Parse timestamp
                    let mut arrived_at = SystemTime::UNIX_EPOCH;
                    let tstr: String = payloads
                        .get("timestamp")
                        .unwrap_or(&String::new())
                        .to_string();
                    if tstr.is_empty() {
                        arrived_at = SystemTime::now();
                    } else {
                        arrived_at += Duration::from_secs(tstr.parse::<u64>().unwrap_or(0));
                    }
                    let arrived_at = Utc.timestamp(
                        arrived_at.duration_since(UNIX_EPOCH).unwrap().as_secs() as i64,
                        0,
                    );
                    // author
                    let author_str = payloads.get("author").unwrap_or(&String::new()).to_string();
                    let author = self.data.profile_manager.display_name(&author_str);
                    // print message
                    if payloads.get("type").unwrap() == "initial" {
                        let mut initial_message = String::from("--> started the conversation");
                        if payloads.get("mode").unwrap_or(&String::new()) == "0" {
                            let uri = self
                                .data
                                .profile_manager
                                .display_name(&payloads.get("invited").unwrap_or(&String::new()));
                            initial_message = String::from(format!(
                                "--> started a private conversation with {}",
                                uri
                            ));
                        }
                        channel
                            .messages
                            .push(Message::new(author, initial_message, arrived_at));
                    } else if payloads.get("type").unwrap() == "text/plain" {
                        channel.messages.push(Message::new(
                            author,
                            String::from(payloads.get("body").unwrap()),
                            arrived_at,
                        ));
                    } else if payloads.get("type").unwrap() == "application/call-history+json" {
                        let duration = payloads
                            .get("duration")
                            .unwrap_or(&String::from("0"))
                            .clone();
                        let duration = duration.parse::<i32>();
                        if duration.is_err() {
                            return Some(());
                        }
                        let duration = duration.unwrap() / 1000;
                        let mut message = format!("📞 Call with duration: {} secs", duration);
                        if duration == 0 {
                            message = format!("❌ Call missed");
                        }
                        channel.messages.push(Message::new(
                            author,
                            String::from(message),
                            arrived_at,
                        ));
                    } else if payloads.get("type").unwrap() == "application/data-transfer+json" {
                        let tid = payloads.get("tid").unwrap_or(&String::new()).clone();
                        let display_name = payloads.get("displayName").unwrap_or(&String::new()).clone();

                        let mut status = String::from("sent");
                        /* Todo: data_transfer_info does not exist anymore in dbus API, will always return None */
                        let info = Jami::data_transfer_info(account_id.clone(), conversation_id.clone(), tid.parse::<u64>().unwrap_or(0));
                        if !info.is_none() {
                            status = match info.unwrap().last_event {
                                3  => String::from("awaiting peer"),
                                4  => String::from("awaiting host"),
                                5  => String::from("ongoing"),
                                6  => String::from("finished"),
                                7  => String::from("closed by host"),
                                8  => String::from("closed by peer"),
                                10 => String::from("unjoinable peer"),
                                11 => String::from("timeout expired"),
                                _  => String::from("not downloaded"),
                            };
                        }

                        let message = match self.data.transfer_manager.path(account_id.clone(), conversation_id.clone(), tid.clone()) {
                            None => format!("<New file transfer with id: {} - {} - {}>", tid, display_name, status),
                            Some(path) => format!("<file://{}>", path),
                        };
                        channel.messages.push(Message::new(
                            author,
                            String::from(message),
                            arrived_at,
                        ));
                    } else if payloads.get("type").unwrap() == "application/update-profile" {
                        // Do not show update infos commits
                        let new_infos = Jami::get_conversation_infos(account_id, conversation_id);
                        channel.update_infos(new_infos);
                    } else if payloads.get("type").unwrap() == "merge" {
                        // Do not show merge commits
                    } else if payloads.get("type").unwrap() == "member" {
                        let action = payloads.get("action");
                        let uri = payloads.get("uri");
                        if action.is_none() || uri.is_none() {
                            return Some(());
                        }
                        let action = String::from(action.unwrap());
                        let uri = String::from(uri.unwrap());
                        let uri = self.data.profile_manager.display_name(&uri);
                        if action == "add" {
                            let msg = format!("--> | {} has been added", uri);
                            channel.messages.push(Message::new(
                                author,
                                String::from(msg),
                                arrived_at,
                            ));
                        } else if action == "join" {
                            let msg = format!("--> | {} joins the conversation", uri);
                            channel.messages.push(Message::new(
                                author,
                                String::from(msg),
                                arrived_at,
                            ));
                        } else if action == "ban" {
                            let msg = format!("<-- | {} was banned from the conversation", uri);
                            channel.messages.push(Message::new(
                                author,
                                String::from(msg),
                                arrived_at,
                            ));
                        } else if action == "remove" {
                            let msg = format!("<-- | {} leaves the conversation", uri);
                            channel.messages.push(Message::new(
                                author,
                                String::from(msg),
                                arrived_at,
                            ));
                        }
                        channel.members = AppData::get_conversations_members(account_id, &conversation_id);
                        for member in &*channel.members {
                            Jami::subscribe_presence(&self.data.account.id, &member.hash, true);
                        }
                    } else {
                        channel.messages.push(Message::new(
                            author,
                            String::from(format!("{:?}", payloads)),
                            arrived_at,
                        ));
                    }
                }
            }
        }
        Some(())
    }

    /**
     * When an account is registered
     * @param self
     * @param _account_id
     * @param registration_state        "REGISTERED" on ready
     */
    pub async fn on_registration_state_changed(
        &mut self,
        _account_id: &String,
        registration_state: &String,
    ) {
        if registration_state == "REGISTERED" && self.data.account == Account::null() {
            self.data.account = Jami::select_jami_account(false);
        }
    }

    /**
     * On presence changed for a member
     */
    pub async fn on_member_presence_changed(&mut self, account_id: &String, uri: &String, flag: bool) {
        if self.data.account.id == *account_id {
            self.data.tracked_presences.insert(uri.to_string(), flag);
        }
    }

    /**
     * Triggered when an account is deleted or added
     */
    pub async fn on_accounts_changed(&mut self) {
        let mut still_there = false;
        for account in Jami::get_account_list() {
            if account.id == self.data.account.id {
                still_there = true;
                break;
            }
        }
        if !still_there {
            // Reselect an account
            self.data.account = Jami::select_jami_account(false);
            if self.data.account.id.is_empty() {
                self.data.channels.state.select(Some(0));
                self.data
                    .channels
                    .items
                    .retain(|channel| channel.id.is_empty());
                self.data.channels.items[0]
                    .messages
                    .push(Message::info(String::from(
                        "!!!! No more account left to use",
                    )));
                return;
            }
            self.data
                .profile_manager
                .load_from_account(&self.data.account.id);
            let channels = AppData::channels_for_account(&self.data.account);
            self.data.channels = StatefulList::with_items(channels);
            if !self.data.channels.items.is_empty() {
                self.data.channels.state.select(Some(0));
            }
            self.data.lookup_members();
        }
    }

    /**
     * Triggered when a contact sent its vCard
     * @param self
     * @param account_id        Receiver
     * @param from              Sender
     * @param path              Path of the profile
     */
    pub async fn on_profile_received(&mut self, account_id: &String, from: &String, path: &String) {
        let dest = get_app_dir(
            AppDataType::UserData,
            &AppInfo {
                name: "jami",
                author: "SFL",
            },
            &*format!("{}/profiles", account_id),
        );
        if dest.is_err() {
            return;
        }
        let dest = dest.unwrap().into_os_string().into_string();
        let dest = format!("{}/{}.vcf", dest.unwrap(), &base64::encode(&*from));
        let result = copy(path, dest.clone());
        if result.is_err() {
            return;
        }
        self.data.profile_manager.load_profile(&dest);
        Jami::lookup_name(&account_id, &String::new(), &from);
    }

    /**
     * When a conversation is loaded
     * @param self
     * @param _id
     * @param account_id
     * @param conversation_id
     * @param messages
     * @return if ok
     */
    pub async fn on_conversation_loaded(
        &mut self,
        _id: u32,
        account_id: String,
        conversation_id: String,
        messages: Vec<HashMap<String, String>>,
    ) -> Option<()> {
        let messages: Vec<_> = messages.into_iter().rev().collect();
        for msg in messages {
            let _ = self.on_message(&account_id, &conversation_id, msg).await;
        }
        Some(())
    }

    pub async fn on_data_transfer_event(
        &mut self,
        account_id: String,
        conversation_id: String,
        tid: u64,
        status: i32
    ) -> Option<()> {
        let info = Jami::data_transfer_info(account_id.clone(), conversation_id.clone(), tid);
        if !info.is_none() {
            let info = info.unwrap();
            match self.data.transfer_manager.path(account_id.clone(), conversation_id.clone(), tid.to_string()) {
                None => {
                    if info.flags == 0 /* outgoing */ {
                        self.data.transfer_manager.set_file_path(account_id.clone(), conversation_id.clone(), tid.to_string(), info.path);
                    } else if status == 6 /* Finished */ {
                        self.data.transfer_manager.set_file_path(account_id.clone(), conversation_id.clone(), tid.to_string(), info.path);
                    }
                },
                _ => {},
            };

            // Note: bad perf there but for now I don't care, will fix this when necessary
            if account_id == &*self.data.account.id {
                if let Some(idx) = self.data.channels.state.selected() {
                    let channel = &mut self.data.channels.items[idx];
                    if channel.id == conversation_id {
                        channel.messages.clear();
                        Jami::load_conversation(&self.data.account.id, &channel.id, &String::new(), 0);
                    }
                }
            }
        }
        Some(())
    }

    /**
     * When a new conversation is ready
     * @param self
     * @param account_id
     * @param conversation_id
     * @return if ok
     */
    pub async fn on_conversation_ready(
        &mut self,
        account_id: String,
        conversation_id: String,
    ) -> Option<()> {
        if account_id == self.data.account.id {
            self.data.channels.state.select(Some(0));
            self.data
                .channels
                .items
                .retain(|channel| channel.id != conversation_id);
            self.data
                .channels
                .items
                .push(Channel::new(&conversation_id, ChannelType::Group));
            self.bubble_up_channel(self.data.channels.items.len() - 1);
            self.data.channels.state.select(Some(0));
        }
        Some(())
    }

    /**
     * When a conversation is removed
     * @param self
     * @param account_id
     * @param conversation_id
     * @return if ok
     */
    pub async fn on_conversation_removed(
        &mut self,
        account_id: String,
        conversation_id: String,
    ) -> Option<()> {
        if account_id == self.data.account.id {
            if let Some(idx) = self.data.channels.state.selected() {
                let channel = &mut self.data.channels.items[idx];
                if channel.id == conversation_id {
                    self.untrack_current_conversation();
                    self.data.channels.state.select(Some(0));
                }
            }
            self.data
                .channels
                .items
                .retain(|channel| channel.id != conversation_id);
        }
        Some(())
    }

    /**
     * When receiving a new conversation request (not a trust request)
     * @param self
     * @param account_id
     * @param conversation_id
     * @todo other parameters?
     * @return if ok
     */
    pub async fn on_conversation_request(
        &mut self,
        account_id: String,
        conversation_id: String,
    ) -> Option<()> {
        if account_id == self.data.account.id {
            self.data
                .channels
                .items
                .push(Channel::new(&conversation_id, ChannelType::Invite));
            self.bubble_up_channel(self.data.channels.items.len() - 1);
            self.data.channels.state.select(Some(0));
        }
        Some(())
    }

    /**
     * When receiving a new trust request
     * @param self
     * @param account_id
     * @param conversation_id
     * @todo other parameters?
     * @return if ok
     */
    pub async fn on_incoming_trust_request(
        &mut self,
        account_id: &String,
        from: &String,
        _payloads: Vec<u8>,
        _receive_time: u64,
    ) -> Option<()> {
        if account_id == &self.data.account.id {
            self.data
                .channels
                .items
                .push(Channel::new(&from, ChannelType::TrustRequest(from.clone())));
            self.bubble_up_channel(self.data.channels.items.len() - 1);
            self.data.channels.state.select(Some(0));
        }
        Some(())
    }

    /**
     * When a name is found, refresh UI if necessary and profile manager
     * @param self
     * @param account_id
     * @param status        0 for success
     * @param address       uri related
     * @param name          name related
     */
    pub async fn on_registered_name_found(
        &mut self,
        account_id: String,
        status: u64,
        address: String,
        name: String,
    ) -> Option<()> {
        self.data.profile_manager.username_found(&address, &name);
        // pending invite
        for i in 0..self.data.out_invite.len() {
            let out_invite = &self.data.out_invite[i];
            if out_invite.account == account_id && out_invite.member == name {
                if status == 0 {
                    if out_invite.channel.as_ref().is_none() {
                        Jami::add_contact(&self.data.account.id, &address);
                        Jami::send_trust_request(
                            &out_invite.account,
                            &address,
                            Vec::new(), /* TODO */
                        );
                    } else {
                        let conversation = out_invite.channel.clone().unwrap();
                        Jami::add_conversation_member(&out_invite.account, &conversation, &address);
                    }
                } else {
                    let channels = &mut self.data.channels.items;
                    for channel in &mut *channels {
                        if channel.id == out_invite.channel.clone().unwrap_or(String::new()) {
                            channel
                                .messages
                                .push(Message::info(String::from("Cannot invite member")));
                        }
                    }
                }
                self.data.out_invite.remove(i);
                break;
            }
        }

        // pending remove
        for i in 0..self.data.pending_rm.len() {
            let pending_rm = &self.data.pending_rm[i];
            if pending_rm.account == account_id && pending_rm.member == name {
                if status == 0 {
                    Jami::rm_conversation_member(
                        &pending_rm.account,
                        &pending_rm.channel,
                        &address,
                    );
                }
                self.data.pending_rm.remove(i);
                break;
            }
        }

        Some(())
    }

    // direct interactions

    fn untrack_current_conversation(&mut self) {
        if let Some(idx) = self.data.channels.state.selected() {
            let channel = &mut self.data.channels.items[idx];
            for member in &*channel.members {
                Jami::subscribe_presence(&self.data.account.id, &member.hash, false);
            }
        }
    }

    fn change_conversation(&mut self, next: bool) {
        self.reset_unread_messages();
        self.untrack_current_conversation();

        if next {
            self.data.channels.next();
        } else {
            self.data.channels.previous();
        }

        if let Some(idx) = self.data.channels.state.selected() {
            let channel = &mut self.data.channels.items[idx];
            if channel.channel_type == ChannelType::Group {
                channel.messages.clear();
                Jami::load_conversation(&self.data.account.id, &channel.id, &String::new(), 0);
            }
            for member in &*channel.members {
                Jami::subscribe_presence(&self.data.account.id, &member.hash, true);
            }
        }
    }

    /**
     * On key up
     */
    pub fn on_up(&mut self) {
        self.change_conversation(false);
    }

    /**
     * On key down
     */
    pub fn on_down(&mut self) {
        self.change_conversation(true);
    }

    /**
     * On key left
     */
    pub fn on_left(&mut self) {
        self.data.input_cursor = self.data.input_cursor.saturating_sub(1);
    }

    /**
     * On key right
     */
    pub fn on_right(&mut self) {
        if self.data.input_cursor < self.data.input.width() {
            self.data.input_cursor += 1;
        }
    }

    /**
     * Clear messages
     */
    fn reset_unread_messages(&mut self) -> bool {
        if let Some(selected_idx) = self.data.channels.state.selected() {
            if self.data.channels.items[selected_idx].unread_messages > 0 {
                self.data.channels.items[selected_idx].unread_messages = 0;
                return true;
            }
        }
        false
    }

    /**
     * Log to file
     */
    #[allow(dead_code)]
    pub fn log(&mut self, msg: impl AsRef<str>) {
        if let Some(log_file) = &mut self.log_file {
            writeln!(log_file, "{}", msg.as_ref()).unwrap();
        }
    }

    /**
     * Move a channel to the top (note that "Jami-cli" will be at the start even after a bubble up)
     * @param channel_idx        Id of the channel to move
     */
    fn bubble_up_channel(&mut self, channel_idx: usize) {
        // bubble up channel to the beginning of the list
        let channels = &mut self.data.channels;
        for (prev, next) in (1..channel_idx).zip(2..channel_idx + 1).rev() {
            channels.items.swap(prev, next);
        }
        match channels.state.selected() {
            Some(0) if 0 == channel_idx => channels.state.select(Some(0)),
            Some(selected_idx) if selected_idx == channel_idx => channels.state.select(Some(1)),
            Some(selected_idx) if selected_idx < channel_idx => {
                channels.state.select(Some(selected_idx + 1));
            }
            _ => {}
        };
    }
}
