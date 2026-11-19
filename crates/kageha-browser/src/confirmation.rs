//! Read-only site confirmation. Never dispatches a click, key, script mutation or navigation.
use crate::{
    actions::Page,
    cdp::{Cdp, Result},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub url: String,
    pub job_id: String,
    pub employer: String,
    pub account_digest: String,
    pub profile: String,
    pub session_digest: String,
    pub target_id: String,
    pub document_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Confirmation {
    pub original_request_id: String,
    pub context: Context,
    pub kind: String,
    pub text: String,
    pub observed_ms: u64,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct Resolution {
    pub request_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Query {
    #[serde(default)]
    pub allow_generic: bool,
    pub browser_id: String,
    pub target_id: String,
    pub binding_id: String,
    pub request_id: Option<String>,
    pub legacy_source: Option<Value>,
}
pub fn job_id(value: &str, fixture: bool) -> Result<String> {
    let url = url::Url::parse(value).map_err(|_| "confirmation_identity_missing")?;
    if !url.username().is_empty() || url.password().is_some() { return Err("confirmation_identity_missing"); }
    let host = url.host_str().unwrap_or("");
    if (url.scheme() == "https" && host.ends_with(".myworkdayjobs.com")
        || fixture && matches!(host,"127.0.0.1"|"localhost")) && url.path().contains("/job/") {
        let parts: Vec<_> = url.path().split('/').collect();
        let job = parts.iter().position(|p| *p == "job").ok_or("confirmation_identity_missing")?;
        let slug = parts.get(job + 2).ok_or("confirmation_identity_missing")?;
        let id = slug.rsplit('_').next().unwrap_or("");
        return if workday_requisition(id) {
            Ok(id.into())
        } else { Err("confirmation_identity_missing") };
    }
    if !((url.scheme() == "https" && matches!(host, "www.linkedin.com" | "linkedin.com"))
        || fixture && matches!(host, "127.0.0.1" | "localhost"))
    {
        return Err("confirmation_provider_unsupported");
    }
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if parts.len() != 3
        || parts[..2] != ["jobs", "view"]
        || parts[2].is_empty()
        || !parts[2].bytes().all(|b| b.is_ascii_digit())
    {
        return Err("confirmation_identity_missing");
    }
    Ok(parts[2].into())
}
fn workday_requisition(id: &str) -> bool {
    id.strip_prefix('R').is_some_and(|tail| !tail.is_empty()
        && tail.split('-').all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit())))
}
/// Only the tenant's exact completion route can inherit an original job identity.
/// Document, tab and session continuity are checked separately by the caller.
pub(crate) fn workday_completion_url(source: &str, current: &str, fixture: bool) -> bool {
    let (Ok(source_url), Ok(current_url)) = (url::Url::parse(source), url::Url::parse(current)) else { return false };
    let Some(host) = source_url.host_str() else { return false };
    if !(source_url.scheme()=="https" && host.ends_with(".myworkdayjobs.com")
        || fixture && matches!(host,"localhost"|"127.0.0.1"))
        || source_url.origin()!=current_url.origin()
        || !source_url.username().is_empty() || source_url.password().is_some()
        || !current_url.username().is_empty() || current_url.password().is_some()
        || job_id(source, fixture).is_err() { return false; }
    let Some(prefix)=current_url.path().strip_suffix("/jobTasks/completed/application") else { return false };
    let Some(source_prefix)=source_url.path().split_once("/job/").map(|v|v.0) else { return false };
    let normalized=|prefix: &str| {
        let parts:Vec<_>=prefix.trim_matches('/').split('/').collect();
        match parts.as_slice() {
            [tenant] if !tenant.is_empty()=>Some((*tenant).to_owned()),
            [locale,tenant] if locale.len()==5 && locale.as_bytes()[2]==b'-'
                && locale.bytes().enumerate().all(|(i,b)|i==2||b.is_ascii_alphabetic()) && !tenant.is_empty()=>Some((*tenant).to_owned()),
            _=>None,
        }
    };
    normalized(source_prefix).is_some_and(|t| normalized(prefix).as_ref()==Some(&t))
}
pub(crate) fn source_job_matches(source: &Context, fixture: bool) -> bool {
    if job_id(&source.url, fixture).ok().as_ref()==Some(&source.job_id) { return true; }
    let Ok(mut u)=url::Url::parse(&source.url) else {return false};
    if job_id(&source.url, fixture).is_err() {return false;}
    u.set_fragment(None);
    source.job_id==format!("url-sha256:{}",data_encoding::HEXLOWER.encode(&Sha256::digest(u.as_str().as_bytes())))
}
pub fn same_job_url(a: &str, b: &str) -> bool {
    let (Ok(a_url), Ok(b_url)) = (url::Url::parse(a), url::Url::parse(b)) else {
        return false;
    };
    let linkedin = |u: &url::Url| {
        u.scheme() == "https" && matches!(u.host_str(), Some("linkedin.com" | "www.linkedin.com"))
    };
    (a_url.origin() == b_url.origin() || linkedin(&a_url) && linkedin(&b_url))
        && job_id(a, true)
            .ok()
            .is_some_and(|id| job_id(b, true).ok().as_ref() == Some(&id))
}
pub fn same_identity(original: &Context, current: &Context) -> bool {
    original.job_id == current.job_id
        && original.employer == current.employer
        && original.profile == current.profile
        && original.session_digest == current.session_digest
        && original.account_digest == current.account_digest
        && (!original.account_digest.is_empty()
            || original.target_id == current.target_id
                && original.document_id == current.document_id)
}
fn completion_bridge<'a>(original:Option<&'a Context>,target:&str,profile:&str,session:&str,document:&str,url:&str,fixture:bool)->Result<Option<&'a Context>> {
    let Some(source)=original.filter(|source|source_job_matches(source,fixture) && workday_completion_url(&source.url,url,fixture)) else {return Ok(None)};
    if source.target_id!=target || source.profile!=profile
        || source.session_digest!=data_encoding::HEXLOWER.encode(&Sha256::digest(session.as_bytes())) {return Err("submission_identity_changed");}
    if source.document_id!=document {return Err("confirmation_document_changed");}
    Ok(Some(source))
}
pub async fn inspect(
    cdp: &mut Cdp,
    target: &str,
    profile: &str,
    session: &str,
    fixture: bool,
    allow_generic: bool,
    original: Option<&Context>,
) -> Result<(Context, Option<(String, String)>)> {
    let tab = cdp.attach(target).await?;
    let tree = cdp.call(Some(&tab), "Page.getFrameTree", json!({})).await?;
    let frame = &tree["frameTree"]["frame"];
    let frame_id = frame["id"].as_str().ok_or("document_changed")?;
    let document = frame["loaderId"]
        .as_str()
        .ok_or("document_changed")?
        .to_owned();
    let source_url = frame["url"].as_str().ok_or("confirmation_identity_missing")?;
    let bridge=match completion_bridge(original,target,profile,session,&document,source_url,fixture) {
        Ok(bridge)=>bridge,
        Err(error)=>{cdp.detach(&tab).await?;return Err(error);},
    };
    if let Err(error) = job_id(source_url, fixture) {
      if bridge.is_none() {
        cdp.detach(&tab).await?;
        if !allow_generic { return Err(error); }
        let mut url=url::Url::parse(source_url).map_err(|_|"confirmation_identity_missing")?;
        if (!fixture && url.scheme()!="https") || !url.username().is_empty() || url.password().is_some() {
            return Err("confirmation_provider_unsupported");
        }
        url.set_fragment(None);
        return Ok((Context{url:source_url.into(),job_id:format!("url-sha256:{}",data_encoding::HEXLOWER.encode(&Sha256::digest(url.as_str().as_bytes()))),
            employer:format!("origin:{}",url.origin().ascii_serialization()),account_digest:String::new(),
            profile:profile.into(),session_digest:data_encoding::HEXLOWER.encode(&Sha256::digest(session.as_bytes())),
            target_id:target.into(),document_id:document},None));
      }
    }
    cdp.detach(&tab).await?;
    let page = Page::from_observation(cdp, target, &json!({"frames":{frame_id:document}})).await?;
    let workday = url::Url::parse(frame["url"].as_str().unwrap_or("")).ok()
        .is_some_and(|u| u.host_str().is_some_and(|h| u.scheme()=="https" && h.ends_with(".myworkdayjobs.com")
            || fixture && matches!(h,"127.0.0.1"|"localhost")) && u.path().contains("/job/"));
    let reply = cdp
        .call(
            Some(&page.session),
            "Runtime.evaluate",
            json!({"contextId":page.context,"returnByValue":true,"expression":if bridge.is_some() {WORKDAY_COMPLETION} else if workday {WORKDAY_INSPECT} else {INSPECT}}),
        )
        .await?;
    page.check_document(cdp).await?;
    page.detach(cdp).await?;
    if reply.get("exceptionDetails").is_some() {
        return Err("confirmation_read_failed");
    }
    let v = &reply["result"]["value"];
    let url = v["url"]
        .as_str()
        .ok_or("confirmation_read_failed")?
        .to_owned();
    if let Some(source)=bridge {
        if !workday_completion_url(&source.url,&url,fixture) { return Err("submission_identity_changed"); }
        let mut context=source.clone();
        context.url=url;
        let confirmation=v["confirmation"]["text"].as_str().filter(|s|!s.is_empty()&&s.len()<=512)
            .map(|text|("workday_completion".into(),text.into()));
        return Ok((context,confirmation));
    }
    let job_id = job_id(&url, fixture)?;
    let employer = v["employer"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 300)
        .ok_or("confirmation_identity_missing")?
        .to_owned();
    let account = v["account"].as_str().unwrap_or("");
    let context = Context {
        url,
        job_id,
        employer,
        account_digest: if account.is_empty() {
            String::new()
        } else {
            data_encoding::HEXLOWER.encode(&Sha256::digest(account.as_bytes()))
        },
        profile: profile.into(),
        session_digest: data_encoding::HEXLOWER.encode(&Sha256::digest(session.as_bytes())),
        target_id: target.into(),
        document_id: document,
    };
    let confirmation = v["confirmation"]["kind"]
        .as_str()
        .zip(v["confirmation"]["text"].as_str())
        .filter(|(kind, text)| {
            matches!(*kind, "sent_modal" | "application_status" | "workday_confirmation")
                && !text.is_empty()
                && text.len() <= 512
        })
        .map(|(kind, text)| (kind.into(), text.into()));
    Ok((context, confirmation))
}
pub fn workday_success_text(text: &str) -> bool {
    let value=text.trim().to_ascii_lowercase();
    ["your application has been submitted", "your application was submitted",
     "your application has been successfully submitted", "your application was successfully submitted",
     "you have successfully applied", "application submitted successfully"]
        .iter().any(|prefix| value.strip_prefix(prefix).is_some_and(|rest|
            rest.is_empty() || rest.starts_with(['.', '!', ' ', '\n'])))
}
const WORKDAY_INSPECT: &str = r#"(()=>{
 const clean=s=>(s||'').replace(/\s+/g,' ').trim();
 const visible=n=>n&&n.checkVisibility({opacityProperty:true,visibilityProperty:true});
 const excluded=n=>n.closest('[data-automation-id="jobPostingDescription"],[data-automation-id="jobApplicationReviewPage"],label');
 if(Array.from(document.querySelectorAll('button')).some(n=>visible(n)&&/^submit$/i.test(clean(n.innerText))))
   return {url:location.href,employer:'workday:'+location.host+location.pathname.split('/job/')[0].replace(/^\/[a-z]{2}-[a-z]{2}(?=\/)/i,''),account:'',confirmation:null};
 const candidates=Array.from(document.querySelectorAll('h1,h2,h3,main p,[role=status],[role=alert],[data-automation-id*="confirmation" i]'));
 let confirmation=null;
 for(const n of candidates){
   if(!visible(n)||excluded(n))continue;
   const text=clean(n.innerText);
   if(text.length<=512 && /^(your application (has been |was )(successfully )?submitted|you have successfully applied|application submitted successfully)([.!\s]|$)/i.test(text)){
     confirmation={kind:'workday_confirmation',text};break;
   }
 }
 const prefix=location.pathname.split('/job/')[0].replace(/^\/[a-z]{2}-[a-z]{2}(?=\/)/i,'');
 return {url:location.href,employer:'workday:'+location.host+prefix,account:'',confirmation};
})()"#;
const WORKDAY_COMPLETION: &str = r#"(()=>{
 const clean=s=>(s||'').replace(/\s+/g,' ').trim();
 const visible=n=>n&&n.checkVisibility({opacityProperty:true,visibilityProperty:true});
 let confirmation=null;
 const activeSubmit=Array.from(document.querySelectorAll('button')).some(n=>visible(n)&&/^submit$/i.test(clean(n.innerText)));
 if(!activeSubmit)for(const dialog of document.querySelectorAll('[role=dialog],dialog')){
   if(!visible(dialog)||dialog.closest('[data-automation-id="jobPostingDescription"],[data-automation-id="jobApplicationReviewPage"]'))continue;
   const heading=Array.from(dialog.querySelectorAll('h1,h2,h3,[role=heading]')).find(n=>visible(n)&&/^application submitted$/i.test(clean(n.innerText)));
   const text=clean(dialog.innerText);
   if(heading&&text.length<=512&&/\b(thank you for your application|you have successfully applied)[.!\s]/i.test(text)){
     confirmation={text};break;
   }
 }
 return {url:location.href,confirmation};
})()"#;
const INSPECT: &str = r#"(()=>{
 const clean=s=>(s||'').replace(/\s+/g,' ').trim();
 const visible=n=>n&&n.checkVisibility({opacityProperty:true,visibilityProperty:true});
 const userContent=n=>n.closest('[id^="JobDetailsJobDescriptionSlot_"],.jobs-description,[class*="jobs-description"],[data-job-description],li,[role=listitem]');
 const accountImage=document.querySelector('img.global-nav__me-photo,.global-nav__me img[alt],[data-test-account]');
 const accountName=clean(accountImage?.getAttribute('data-test-account')||accountImage?.getAttribute('alt'));
 const accountLink=document.querySelector('nav a[href*="/in/"][aria-label*="profile" i],.global-nav__me a[href*="/in/"]');
 const account=accountLink?.href || (/^(me|profile|profile photo|avatar)$/i.test(accountName)?'':accountName);
 const main=document.querySelector('main,[role=main]');
 if(!main)throw Error('missing job');
 const company=Array.from(main.querySelectorAll('a[href*="/company/"],[data-test-job-company-name]')).find(n=>visible(n)&&clean(n.innerText));
 const employer=clean(company?.innerText);
 let confirmation=null;
 const dialogs=Array.from(document.querySelectorAll('[role=dialog],dialog')).filter(visible);
 for(const dialog of dialogs){
   const headings=Array.from(dialog.querySelectorAll('h1,h2,h3,[role=heading]')).filter(n=>visible(n)&&!userContent(n));
   for(const h of headings){const text=clean(h.innerText);if(employer&&text.toLowerCase()===('Your application was sent to '+employer+'!').toLowerCase())confirmation={kind:'sent_modal',text};}
 }
 if(!confirmation){
   const headings=Array.from(main.querySelectorAll('h2,h3,[role=heading]')).filter(n=>visible(n)&&clean(n.innerText)==='Application status'&&!userContent(n));
   if(headings.length===1){
     let parent=headings[0].parentElement;
     for(let i=0;parent&&i<3;i++,parent=parent.parentElement){
       if(clean(parent.innerText).length>400)break;
       const matches=Array.from(parent.querySelectorAll('span,p,div')).filter(n=>visible(n)&&clean(n.innerText)==='Application submitted');
       if(matches.length){confirmation={kind:'application_status',text:'Application submitted'};break;}
     }
   }
 }
 return {url:location.href,employer,account,confirmation};
})()"#;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workday_identity_and_success_are_specific() {
        let url="https://web.wd1.myworkdayjobs.com/en-US/Careers/job/Ontario/Engineer_R15157/apply";
        assert_eq!(job_id(url,false).unwrap(),"R15157");
        for bad in ["https://web.wd1.myworkdayjobs.com.evil.test/en-US/Careers/job/Ontario/Engineer_R15157/apply",
            "https://user:secret@web.wd1.myworkdayjobs.com/en-US/Careers/job/Ontario/Engineer_R15157/apply",
            "https://web.wd1.myworkdayjobs.com/en-US/Careers/job/Ontario/Engineer_fake/apply"] {
            assert!(job_id(bad,false).is_err());
        }
        assert!(workday_success_text("Your application has been successfully submitted."));
        for text in ["Your application will be submitted", "Your application has not been submitted", "Your application has been submittedish", "Review your application"] {
            assert!(!workday_success_text(text));
        }
    }
    #[test]
    fn job_identity_is_strict() {
        assert_eq!(
            job_id(
                "https://www.linkedin.com/jobs/view/4489586201/?trk=x",
                false
            )
            .unwrap(),
            "4489586201"
        );
        for u in [
            "https://evil.test/jobs/view/4489586201/",
            "https://www.linkedin.com/jobs/search/",
            "http://www.linkedin.com/jobs/view/1/",
        ] {
            assert!(job_id(u, false).is_err());
        }
    }
    #[test]
    fn workday_completion_retains_exact_requisition_and_tenant() {
        let source="https://employer.wd103.myworkdayjobs.com/en-US/Careers/job/Toronto/Lead_R12345-1/apply";
        let complete="https://employer.wd103.myworkdayjobs.com/en-US/Careers/jobTasks/completed/application";
        assert_eq!(job_id(source,false).unwrap(),"R12345-1");
        assert!(workday_completion_url(source,complete,false));
        for bad in [complete.replace("Careers","AnotherTenant"),complete.replace("employer.","another."),complete.replace("https:","http:"),format!("{complete}/extra"),complete.replace("application","review")] {
            assert!(!workday_completion_url(source,&bad,false),"{bad}");
        }
        for bad in ["R12345-", "R12345-foo", "R12345--1", "R-1"] {assert!(!workday_requisition(bad));}
        let mut original=Context {url:source.into(),job_id:"R12345-1".into(),employer:"Employer".into(),account_digest:String::new(),profile:"profile".into(),session_digest:"session".into(),target_id:"tab".into(),document_id:"document".into()};
        assert!(source_job_matches(&original,false));
        original.session_digest=data_encoding::HEXLOWER.encode(&Sha256::digest(b"session"));
        assert!(completion_bridge(Some(&original),"tab","profile","session","document",complete,false).unwrap().is_some());
        assert_eq!(completion_bridge(Some(&original),"tab","profile","session","replacement-document",complete,false).err(),Some("confirmation_document_changed"));
        original.job_id=format!("url-sha256:{}",data_encoding::HEXLOWER.encode(&Sha256::digest(source.as_bytes())));
        assert!(source_job_matches(&original,false));
        original.job_id="url-sha256:wrong".into();assert!(!source_job_matches(&original,false));
    }
}
