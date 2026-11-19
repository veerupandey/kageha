# Shopify integration

Settings → Integrations → Shopify supports multiple stores, encrypted credentials, per-store agent read/edit-preparation grants, and a product edit review queue. Administrator setup fields replace environment-variable requirements.

## Authentication choice

The connector is a standalone app, so **merchant OAuth authorization-code grant** is the default. Use **client credentials** only when the app and installed stores belong to the same Shopify organization. This follows [Shopify's authentication guidance](https://shopify.dev/docs/apps/build/authentication-authorization). New setup uses the Dev Dashboard; the old admin-created custom-app token instructions do not apply to new apps.

1. Create and distribute/install a Shopify app with the appropriate Dev Dashboard configuration.
2. Enter Client ID and Secret, authentication mode, exact public HTTPS redirect URI, and a supported API version in Settings. The default is `2026-07`.
   In the Shopify Dev Dashboard, open the app, then **Versions → Create version**. The URL fields are versioned there and may not appear under the app's general Settings. Set **Application URL** to the same host as the redirect URI. For example, if the callback is `https://app.example.com/api/integrations/shopify/callback`, the Application URL must use `https://app.example.com`. A temporary preview host can work for testing, but both values must use that exact preview host; use a stable production domain before distributing the app.
   Save and release/publish that version before using its install flow; an edited draft version is not necessarily the version Shopify uses for installation.
3. Register the redirect URI and configure the same desired scopes in Shopify. Product reading is the default. Inventory items, order summaries, and reviewed product edits are optional.
4. Enter the store's canonical `name.myshopify.com` hostname and connect. OAuth users approve in Shopify; own-organization users must have installed the app first.
5. Assign agents to that particular store. Product-edit preparation requires both read access and `write_products`.

OAuth callbacks validate HMAC, a one-time state, expiry, and the exact expected shop. Token exchange requests expiring offline access and saves refresh tokens encrypted. Refresh occurs on use before expiry under a process lock; expired/revoked refresh credentials require reconnection. Client-credential connections acquire a fresh token on use near expiry. [Shopify's standalone app flow](https://shopify.dev/docs/apps/build/authentication-authorization/authenticate-standalone-apps) documents this exchange and validation.

## Supported operations

- `shopify_stores`: list stores assigned to the calling agent.
- `shopify_read`: store information, products, inventory item IDs/SKUs/tracking flags, and order summaries with financial/fulfillment status and totals. Results use bounded cursor pagination and Shopify search syntax. Inventory quantities/location management are not included.
- `shopify_prepare_product_update`: prepare product title, HTML description, or tag changes. The owner sees before/after values and approves in Settings. The product is re-read before mutation; changes since preparation reject the edit. Stable request keys avoid duplicate preparations and ambiguous mutation responses cannot be automatically replayed.

There is no arbitrary GraphQL tool, customer contact-data query, refund, payment, price, fulfillment, or inventory mutation. Order access depends on Shopify's permissions/data-access approval and normal order-history limits. Product edits use [Shopify's `productUpdate` mutation](https://shopify.dev/docs/api/admin-graphql/2026-07/mutations/productUpdate); variants and pricing use different mutations and are outside this implementation.

## Webhooks and deployment

Register the Settings webhook URL for `app/uninstalled`, `app/scopes_update`, and mandatory privacy topics (`customers/data_request`, `customers/redact`, `shop/redact`) in the Shopify app configuration. This deployment does not register subscriptions automatically. The receiver validates the body HMAC. Uninstall and shop-redaction clear local store records; scope notifications can only reduce local scopes. Customer privacy events retain only anonymous bounded receipt metadata because this connector does not fetch customer identities. Privacy handling for independent thread content, logs, backups, and any other app features remains a deployment responsibility; this receiver alone is not proof of full-app privacy compliance.

Store records, pending OAuth, action history and credentials live in encrypted integration files. Protect and back up the encryption key. Changing Client ID clears local store connections. Disconnect removes local records; uninstall the app in Shopify to revoke provider access.

The application currently uses a single-owner, single-backend-worker trust model. Put Settings and agent APIs behind authenticated access. Multi-tenant hosting needs tenant-scoped credential storage and server-enforced roles before release. Configure your proxy to allow provider callbacks without exposing owner APIs.

For a downloaded/self-hosted Vite development server, set `VITE_ALLOWED_HOSTS` to a comma-separated list of hostnames used by your reverse proxy. The integration does not assume a Cloudspaces, Hyperagent, localhost, or other vendor domain; its default callback is derived from the URL currently open in Settings. Use **Use this installation URL** after moving the app to a new host, then register the resulting callback in Shopify.

Before a live release, verify installation/distribution, OAuth and own-organization modes as applicable, token refresh, signed callbacks, scoped reads, per-store grants, uninstall, and an explicitly approved test-product edit. Confirm Shopify app review and protected data requirements for the chosen distribution. Unit tests mock Shopify and cannot establish live approval or successful installation.
