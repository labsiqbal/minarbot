# Receive agent email

1. On an install with agent email support, open **Settings → Email** and save a domain you control, such as `agents.example.com`. Choose **Generate secret** and copy the value when it appears. Store it securely. Rotating it requires updating the Worker secret.
2. Publish an HTTPS URL for `POST /api/email/inbound` through a reverse proxy or Cloudflare Tunnel on the Minarbot host. Forward only this path. Keep the upstream Host header set to the local Minarbot address or an existing allowed origin. For a loopback HTTP install on port 4317, a locally managed tunnel can use these ingress rules.

   ```yaml
   ingress:
     - hostname: minarbot-mail.example.com
       path: '^/api/email/inbound$'
       service: http://127.0.0.1:4317
       originRequest:
         httpHostHeader: 127.0.0.1:4317
     - service: http_status:404
   ```

   For a new locally managed tunnel, run `cloudflared tunnel login` and `cloudflared tunnel create minarbot-mail`. In its config, set `tunnel` to the returned ID, `credentials-file` to the generated JSON path, and add these ingress rules. Run `cloudflared tunnel route dns minarbot-mail minarbot-mail.example.com` and `cloudflared tunnel --config <config.yml> run minarbot-mail`. For a TLS listener, set the upstream to HTTPS and configure the certificate name and any private CA. Verify that the relay hostname serves only the inbound path. [Cloudflare Tunnel ingress](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/do-more-with-tunnels/local-management/configuration-file/)
3. Create a Cloudflare Worker in **Workers & Pages**, replace its code with [worker.mjs](worker.mjs), and deploy it. Under **Settings → Variables and Secrets**, set `MAIL_ENDPOINT` to `https://minarbot-mail.example.com/api/email/inbound`. Add `MAIL_SECRET` as a **Secret** using the exact value from Minarbot. Never add it to the Worker source.
4. Open **Compute → Email Service → Email Routing** and onboard the receiving domain. Apply the requested DNS records. Under **Routing Rules**, choose **Send to a Worker** for your agent addresses. Use the **Catch-all address** action on a domain dedicated to Minarbot to route all agents, including new ones. [Cloudflare Email Routing](https://developers.cloudflare.com/email-service/get-started/route-emails/)
5. In Minarbot, open an agent's action menu and choose **Inbox**. Copy its address and send a test message from a separate mailbox. Confirm it appears in that inbox. An address with an unknown local part should bounce.

Email tools run in private local agent turns. Inbound relays can post up to 2 MiB of raw RFC 5322 content. Minarbot stores up to 32,000 characters of message text. Outbound approval saves a draft for a future transport. For cross-host recovery, keep the private `<database>.agent-mail-key` sidecar with the database.

## Other SMTP-to-HTTP relays

Use the SMTP envelope recipient, including for Bcc. Send the original bytes with `Content-Type: message/rfc822`. Set `X-Minarbot-Email-To` to the envelope recipient and `X-Minarbot-Email-Timestamp` to Unix seconds. Set `X-Minarbot-Email-Signature` to unpadded base64url HMAC-SHA256 with the UTF-8 endpoint secret. Sign this UTF-8 prefix followed by the exact message bytes.

```text
minarbot-email-v1\n<TIMESTAMP>\n<RECIPIENT>\n
```

Minarbot accepts timestamps within five minutes. HTTP 204 confirms delivery or a duplicate. HTTP 404 rejects an unknown recipient. HTTP 401 rejects invalid signatures or timestamps. For OTP matching, Minarbot uses the From header. Apply sender authentication at the receiving relay.

## Parser provenance

`mail-parser` from Stalwart Labs handles MIME and RFC 5322 decoding. The 0.11 series supports this project's Rust toolchain. It and its transitive dependency `hashify` use Apache-2.0 OR MIT. [mail-parser source](https://github.com/stalwartlabs/mail-parser) · [hashify source](https://github.com/stalwartlabs/hashify)
