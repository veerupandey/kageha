# Instagram Business integration

Settings → Integrations → Instagram provides direct Instagram Login setup, OAuth connection, agent access, and an action review queue. No environment variables are required. This integration is intended for an Instagram professional account; Business accounts can use Stories publishing where Meta permits it.

## Configure

1. Create a Meta developer app with Instagram API and Instagram Login. Copy the **Instagram App ID and App Secret** into Settings. These are the Instagram product credentials, not a GitHub or Google client.
2. Register the exact HTTPS redirect URI displayed in Settings. Its origin must be the public application origin; a different hostname, protocol, port, or trailing slash causes OAuth errors.
3. Select the features your app is permitted to request. Reading uses `instagram_business_basic`; publishing, comments, and messaging request their respective `instagram_business_content_publish`, `instagram_business_manage_comments`, and `instagram_business_manage_messages` permissions.
4. Save setup, connect your Business account, and save agent permissions. Read access and permission to prepare actions are separate. Agents cannot execute prepared actions themselves through the integration tools.
5. For notifications, set a random verify token of at least 16 characters. Register the displayed webhook callback and the same token in Meta, configure the relevant fields there, then click **Enable account webhooks** after connecting. Notifications are signature-checked and deduplicated; only receipt metadata is stored. They do not automatically run an agent or send a reply.
6. Register the displayed deauthorization and data deletion callbacks. These accept Meta signed requests. Deletion clears the matching connected account and local action history, retaining an anonymous completion receipt for up to 30 days. App configuration remains available for a new connection.

## Agent workflow

`instagram_account_read` reads the connected profile, media, comments, selected media insights, conversations/messages, and publishing quota where granted and supported. Results are untrusted external content. Insights availability varies by media type and Meta API version; provider rejection is reported as an error.

`instagram_prepare_action` prepares posts, comment replies/moderation, or customer replies using a stable request key. The owner reviews the exact payload in Settings and approves execution. Images/videos must already have publicly accessible HTTPS URLs that Meta can fetch. Carousels accept 2–10 images/videos. No scheduler or media hosting service is included.

Media processing can require another **Check media and publish** click. Saved container IDs prevent duplicate container creation during normal resume. An ambiguous timeout or a server interruption leaves an uncertain/running record and blocks automatic retries. Check Instagram directly before preparing a replacement: this integration cannot prove whether Meta accepted a write whose response was lost.

Messaging is limited to existing conversations and a recent inbound customer message within the standard 24-hour window. No cold outreach or human-agent window extension is implemented.

## Operations and release verification

Secrets, tokens, grants, pending OAuth states, and action records are encrypted under the application integration directory. Back up its encryption key with restricted access. This follows the existing **single-owner deployment** model: serve Settings and agent APIs behind authenticated access. It does not implement tenant isolation. Expose only the required OAuth and signed Meta callback paths to unauthenticated provider requests through your proxy.

Long-lived tokens refresh on use when fewer than seven days remain. An account unused until expiry must reconnect. There is no background renewal job. Changing the app ID clears the previous connection and local action history; transient server errors should be retried, not handled by resetting credentials.

Before calling a deployment production-ready:

- Complete Meta review/advanced access and any required business verification for accounts outside the app's development roles. Supply the privacy policy and deletion information required by Meta.
- Confirm the configured Graph API version is supported for your app and selected features.
- Verify OAuth, permission denial, reconnection, webhook challenge/signature, deauthorization, and deletion against the real app.
- Using explicitly approved test content, verify each enabled publish/comment/message operation, processing resume, and permission revocation with the real account.
- Confirm authenticated deployment access, durable encrypted storage, backups, and HTTPS routing.

Local tests mock Meta. Passing them does not establish Meta app approval or successful live publishing. Reference: [Meta's Instagram API with Instagram Login collection](https://www.postman.com/meta/instagram/folder/6raa77c/instagram-api-with-instagram-login).
