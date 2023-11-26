use env_logger;
use env_logger::Env;
use imap;
use regex::Regex;
use rustls_connector::rustls::{ClientConnection, StreamOwned};
use rustls_connector::RustlsConnector;
use selenium_manager::logger::Logger;
use selenium_manager::{get_manager_by_browser, SeleniumManager};
use serde::{Deserialize, Serialize};
use std::error::Error;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::process::Command;
use thirtyfour::prelude::*;
use tokio::time::{sleep, Duration};

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Settings {
    gmail_username: String,
    gmail_oauth2: String,

    register_password: String,

    postfix: String,
    start_num: u32,
    end_num: u32,
}

struct GmailOAuth2 {
    user: String,
    access_token: String,
}

impl GmailOAuth2 {
    fn new<T, U>(user: &T, access_token: &U) -> Self
    where
        T: AsRef<str>,
        U: AsRef<str>,
    {
        Self {
            user: user.as_ref().to_string(),
            access_token: access_token.as_ref().to_string(),
        }
    }
}

impl imap::Authenticator for GmailOAuth2 {
    type Response = String;

    fn process(&self, _data: &[u8]) -> Self::Response {
        format!(
            "user={}\x01auth=Bearer {}\x01\x01",
            self.user, self.access_token
        )
    }
}

trait EmailWaiterTrait {
    fn check_inbox(&mut self) -> Result<(), Box<dyn Error>>;
    fn check_verify_code(&mut self) -> Result<Option<String>, Box<dyn Error>>;
}

struct EmailWaiter<T>
where
    T: Read + Write,
{
    sess: imap::Session<T>,
    exist_mail_count: u32,
}

impl EmailWaiterTrait for EmailWaiter<StreamOwned<ClientConnection, TcpStream>> {
    fn check_inbox(&mut self) -> Result<(), Box<dyn Error>> {
        let mailbox = self.sess.select("INBOX")?;
        self.exist_mail_count = mailbox.exists;
        Ok(())
    }

    fn check_verify_code(&mut self) -> Result<Option<String>, Box<dyn Error>> {
        let mailbox = self.sess.select("INBOX")?;
        let mut ret: Option<String> = None;
        if mailbox.exists > self.exist_mail_count {
            let mails = self
                .sess
                .fetch(format!("{}", mailbox.exists), "(UID BODY[TEXT])")?;
            for msg in &mails {
                let opt_code = Self::try_get_code(msg);
                if let Some(code) = opt_code {
                    ret = Some(code);
                    break;
                }
            }
            self.exist_mail_count = mailbox.exists;
        }
        Ok(ret)
    }
}

impl EmailWaiter<StreamOwned<ClientConnection, TcpStream>> {
    fn from_setting(settings: &Settings) -> Result<Self, Box<dyn Error>> {
        let gmail_auth = GmailOAuth2::new(&settings.gmail_username, &settings.gmail_oauth2);
        let host = "imap.gmail.com";
        let stream = TcpStream::connect((host, 993))?;
        let tls = RustlsConnector::new_with_native_certs()?;
        let tlsstream = tls.connect(host, stream)?;
        let client = imap::Client::new(tlsstream);
        let sess = client
            .authenticate("XOAUTH2", &gmail_auth)
            .map_err(|e| e.0)?;
        Ok(Self {
            sess,
            exist_mail_count: 0,
        })
    }

    fn try_get_code(fetch: &imap::types::Fetch) -> Option<String> {
        let mut ret: Option<String> = None;
        if let Some(text_byte) = fetch.text() {
            if let Ok(text) = std::str::from_utf8(text_byte) {
                let re = Regex::new(r"eaeaea.+>(?<code>[0-9]{6})</span>").unwrap();
                if let Some(caps) = re.captures(text) {
                    let code = &caps["code"];
                    println!("Get code: {}", code);
                    ret = Some(code.to_string());
                }
            }
        }

        ret
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::Builder::from_env(Env::default().default_filter_or("debug")).init();
    let mut manager: Box<dyn SeleniumManager> = get_manager_by_browser("chrome".to_string())?;
    manager.set_logger(Logger::create("LOGGER", true, false));
    manager.set_avoid_browser_download(true);

    manager.setup()?;
    let brow_name = manager.get_browser_name();
    let brow_path = manager.get_browser_path();
    let brow_ver = manager.get_browser_version();
    let driv_ver = manager.get_driver_version();
    let driv_path = manager.get_driver_path_in_cache()?;

    println!("Browser ver: {}", brow_ver);
    println!("Browser name: {}", brow_name);
    println!("Browser path: {}", brow_path);
    println!("Driver ver: {}", driv_ver);
    println!("Driver path: {:?}", driv_path);

    println!("Start webdriver");
    let mut proc = Command::new(driv_path).spawn()?;
    let ret = start_async_main();
    println!("Kill webdriver");
    proc.kill()?;
    ret
}

fn start_async_main() -> Result<(), Box<dyn Error>> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async_main())?;
    Ok(())
}

async fn async_main() -> Result<(), Box<dyn Error>> {
    let settings_str = fs::read_to_string("settings.yaml")?;
    let settings = serde_yaml::from_str(settings_str.as_str())?;
    println!("{:?}", settings);
    let mut email_waiter = EmailWaiter::from_setting(&settings)?;
    let caps = DesiredCapabilities::chrome();
    let driver = WebDriver::new("http://localhost:9515", caps).await?;

    let ret = run(&driver, &settings, &mut email_waiter).await;

    if let Err(err) = &ret {
        println!("Error {:?}", err);
        sleep(Duration::from_secs(10)).await;
    }

    driver.quit().await?;
    ret
}

async fn sleep_in_sec(t: u64) {
    sleep(Duration::from_secs(t)).await;
}

async fn run_once(
    driver: &WebDriver,
    email_waiter: &mut impl EmailWaiterTrait,
    email: &str,
    passwd: &str,
) -> Result<(), Box<dyn Error>> {
    driver.goto("https://greenlifevideo.net/").await?;

    let btn_login_parent = driver.query(By::ClassName("btn-login")).first().await?;
    let login_btn = btn_login_parent.query(By::Tag("a")).first().await?;
    let btn_class = login_btn.attr("class").await?;

    if btn_class == Some(String::from("btn_logout")) {
        println!("Already logged in, logout first");
        login_btn.click().await?;
        sleep_in_sec(3).await;
    }

    let btn_login_parent = driver.query(By::ClassName("btn-login")).first().await?;
    let login_btn = btn_login_parent.query(By::Tag("a")).first().await?;

    println!("Start to register");
    login_btn.click().await?;
    sleep_in_sec(3).await;

    println!("Open register form");
    let register_btn = driver
        .query(By::Css("button[data-bs-target='#registerModal']"))
        .first()
        .await?;
    register_btn.click().await?;
    sleep_in_sec(3).await;

    let register_form = driver.query(By::Id("form_register")).first().await?;
    let input_reg_email = register_form
        .query(By::Id("inputRegisterEmail"))
        .first()
        .await?;
    let input_reg_pass1 = register_form
        .query(By::Id("inputRegisterPassword1"))
        .first()
        .await?;
    let input_reg_pass2 = register_form
        .query(By::Id("inputRegisterPassword2"))
        .first()
        .await?;
    let input_reg_check1 = register_form
        .query(By::Id("registerCheck1"))
        .first()
        .await?;
    let btn_submit = register_form
        .query(By::Css("button[type='submit']"))
        .first()
        .await?;

    input_reg_email.click().await?;
    // let reg_email = format!(
    //     "{}+{}{:0>3}@gmail.com",
    //     settings.gmail_username, settings.postfix, settings.start_num
    // );
    let reg_email = email.to_string();
    let reg_passwd = passwd.to_string();
    println!("Register email: {}", reg_email);
    println!("Fill in form");
    input_reg_email.send_keys(reg_email.as_str()).await?;
    input_reg_pass1.click().await?;
    input_reg_pass1.send_keys(reg_passwd.as_str()).await?;
    input_reg_pass2.click().await?;
    input_reg_pass2.send_keys(reg_passwd.as_str()).await?;
    input_reg_check1.click().await?;
    sleep_in_sec(1).await;

    println!("Check inbox");
    email_waiter.check_inbox()?;

    btn_submit.click().await?;
    sleep_in_sec(5).await;

    println!("Find form_register_verify");
    let form_register_verify = driver.query(By::Id("form_register_verify")).first().await?;
    let input_verify_code = form_register_verify
        .query(By::Id("inputRegisterVerifyCode"))
        .first()
        .await?;
    let btn_submit = form_register_verify
        .query(By::Css("button[type='submit']"))
        .first()
        .await?;

    let mut opt_code: Option<String> = None;
    for i in 0..10 {
        println!("Try get code, {} times", i);
        opt_code = email_waiter.check_verify_code()?;
        if opt_code.is_some() {
            break;
        }
        sleep_in_sec(10).await;
    }
    let code = opt_code.ok_or("Fail to get code from email")?;

    input_verify_code.click().await?;
    input_verify_code.send_keys(code.as_str()).await?;
    sleep_in_sec(1).await;
    btn_submit.click().await?;
    sleep_in_sec(5).await;

    println!("Find registerSuccessModal");
    let register_success = driver.query(By::Id("registerSuccessModal")).first().await?;
    let reg_login_btn = register_success
        .query(By::Css("button[data-bs-target='#loginModal']"))
        .first()
        .await?;
    reg_login_btn.click().await?;
    sleep_in_sec(3).await;

    println!("Find form_login");
    let login_form = driver.query(By::Id("form_login")).first().await?;
    let input_email = login_form.query(By::Id("inputLoginEmail")).first().await?;
    let input_passwd = login_form
        .query(By::Id("inputLoginPassword"))
        .first()
        .await?;
    let btn_submit = login_form
        .query(By::Css("button[type='submit']"))
        .first()
        .await?;

    input_email.click().await?;
    input_email.send_keys(reg_email.as_str()).await?;
    input_passwd.click().await?;
    input_passwd.send_keys(reg_passwd.as_str()).await?;
    sleep_in_sec(1).await;
    btn_submit.click().await?;
    sleep_in_sec(3).await;

    println!("Find vote button");
    let video_block = driver.query(By::Css("div[data-id='24']")).first().await?;
    video_block.scroll_into_view().await?;
    sleep_in_sec(2).await;
    let vote_btn = video_block
        .query(By::Css("button.btn-outline-vote"))
        .first()
        .await?;
    vote_btn.scroll_into_view().await?;
    sleep_in_sec(2).await;
    vote_btn.click().await?;

    println!("done");
    sleep_in_sec(5).await;
    Ok(())
}

async fn run(
    driver: &WebDriver,
    settings: &Settings,
    email_waiter: &mut impl EmailWaiterTrait,
) -> Result<(), Box<dyn Error>> {
    let start = settings.start_num;
    let end = settings.end_num;

    for num in start..end {
        let reg_email = format!(
            "{}+{}{:0>3}@gmail.com",
            settings.gmail_username, settings.postfix, num
        );
        let reg_passwd = settings.register_password.as_str();
        run_once(driver, email_waiter, reg_email.as_str(), reg_passwd).await?;
    }

    Ok(())
}
