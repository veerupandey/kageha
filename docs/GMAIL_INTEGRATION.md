# Gmail integration

Gmail appears in Settings → Integrations alongside GitHub. This first release
supports one mailbox per Kageha home, explicitly assigned agents, and read-only
email search, message reading, and thread reading. It does not send, delete,
archive, change labels, or mark messages as read. Attachments are listed but
not downloaded or parsed.

## Configure Google sign-in

1. Create or select a project in https://console.cloud.google.com/ and enable
   the Gmail API.
2. Configure Google Auth Platform branding and audience. During development,
   use Testing and add the Google accounts that will connect as test users.
3. Configure the `https://www.googleapis.com/auth/gmail.readonly` scope.
4. Create an OAuth client of type **Web application**.
5. Add an authorized redirect URI that reaches this Kageha instance, ending
   in `/api/integrations/gmail/callback`. For local use, for example:
   `http://localhost:8788/api/integrations/gmail/callback`. For a remotely
   accessed server, use its public HTTPS URL, not localhost. The Vite frontend
   proxies `/api`, so its public URL can also be used.
6. In **Settings → Integrations → Gmail → Google OAuth setup**, enter the Client ID,
   Client Secret, and redirect URI, or import the downloaded Web client JSON.
   Save setup, then click Connect with Google. No restart is required.

Saved setup is encrypted in `integrations/google_config.enc` and overrides the
environment configuration below. A blank secret keeps the existing secret when
the Client ID is unchanged. Changing the Client ID clears the old mailbox and
agent assignments. Disconnecting Gmail preserves provider setup.

Environment configuration remains available as an alternative:

```dotenv
KAGEHA_GOOGLE_CLIENT_ID=your-web-client-id.apps.googleusercontent.com
KAGEHA_GOOGLE_CLIENT_SECRET=your-client-secret
KAGEHA_GOOGLE_REDIRECT_URI=https://your-kageha-host/api/integrations/gmail/callback
```

The redirect URI must exactly match Google's configuration. Keep the secret
outside version control and frontend environment variables. Restart the backend
after environment configuration. Saved client secrets are never returned to the browser.

## Connect and use

Open Settings → Integrations → Gmail → Connect with Google. Approve access in
the new tab. The original Settings panel updates automatically; if the popup
is blocked, use its Open Google sign-in link. Select agents and save access.

Try: “Summarize unread emails in my inbox” or “Find emails from
person@example.com about the contract and read the latest thread.”

The `gmail_read` core tool provides `profile`, `search`, `message`, and `thread`
operations. Search supports Gmail query syntax, limits of 1–25 results, and
pagination with `page_token`. Each search result includes message/thread IDs,
headers, a snippet, and a Gmail link. Message text is capped at 50,000 characters;
threads return at most 20 messages and report truncation. Gmail links may require
switching to the connected Google account if the browser has multiple accounts.

## Account lifecycle

OAuth uses expiring one-time state, PKCE, offline access, and server-side token
exchange. Tokens are automatically refreshed when needed. A revoked or expired
refresh token requires reconnection. Google may apply shorter refresh-token
lifetimes to apps in Testing; this is not a production verification substitute.

Credentials are encrypted in `$KAGEHA_HOME/integrations/gmail.enc` with the
owner-readable local integrations key. Local account compromise is outside this
storage protection. The existing local-owner trust model applies; agent grants
restrict this connector, not arbitrary host filesystem access through other tools.
Every tool call checks current agent grants. Changing Google accounts clears
previous assignments. Cancelling sign-in invalidates pending authorization.
Disconnect deletes local credentials and pending authorization immediately.
Remove Kageha access in your Google account separately to revoke Google's grant.

The Gmail read-only scope is restricted. Public distribution/production use may
require Google's verification and security assessment depending on usage; this
release does not implement that administrative process or a hosted OAuth service.

References:
- https://developers.google.com/identity/protocols/oauth2/web-server
- https://developers.google.com/workspace/gmail/api/auth/scopes
- https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages/list
