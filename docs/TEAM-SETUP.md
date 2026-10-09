# Team sign in and Source Control

## Link the local owner to GitHub

Local Sift works without sign-in and remains usable offline. To identify its
owner with GitHub:

1. Register your own GitHub OAuth App and enable **Device flow**. Sift does not
   bundle a shared OAuth registration or require a local client secret.
2. For a normal local server, put the public client ID in its private `.env`:
   `SIFT_AUTH__GITHUB_DEVICE_CLIENT_ID=<your OAuth App client ID>`.
   For an applied personal instance, set `auth.github.client_id` in `sift.toml`
   with `flow = "local-device"`, review/apply the configuration, then restart.
   **Edit authentication setup…** in Account opens that manifest section.
3. Open **Account → Link GitHub owner**. Copy the displayed code, enter it on
   GitHub, and authorize the app. Account provides **Copy code**, **Open GitHub**,
   and **Cancel**. Closing Account cancels the desktop flow.
4. GitHub is linked to the existing Sift principal. Its IDs, connections,
   documents, and personal tenant remain intact. The original local bootstrap
   owner becomes the first instance administrator only if no active admin
   already exists. An applied instance's declared GitHub subject must match;
   signing in with another account cannot replace it.

This flow requires verified local OS ownership. It cannot claim an instance
through the network or SSH, enable hosting, or bypass an existing owner.
**Verify GitHub owner** repeats verification for a linked account.

If GitHub is unavailable, local OS access still works. Owner linking does not
remove local recovery or require an internet connection for ordinary local use.
Hosted password recovery remains available through the operator's
`sift-admin` tooling; recovery never promotes an arbitrary remote visitor.

## Add people to a hosted server

Sift has its own principals. A GitHub account becomes a Sift principal only
after an instance administrator admits that GitHub login. GitHub sign-in does
not grant access to an existing team room by itself.

1. Configure a team/network instance through the reviewed instance setup flow.
   Local owner linking alone does not expose your database server to others.
   For a non-instance development server, bootstrap the first administrator with
   `sift-admin bootstrap-admin <username> --password-stdin`. Applied instances
   declare their bootstrap GitHub owner in [instance configuration](INSTANCE-CONFIG.md).
2. Configure a GitHub OAuth App for that server. Set its callback URL to
   `<public HTTPS origin>/v1/auth/github/callback`, and set
   `SIFT_AUTH__PUBLIC_BASE_URL`, `SIFT_AUTH__GITHUB_CLIENT_ID`, and
   `SIFT_AUTH__GITHUB_CLIENT_SECRET` in the server's private `.env`.
   Applied instances use `flow = "hosted-code"` and the typed OAuth credential
   slot described in [instance configuration](INSTANCE-CONFIG.md).
3. Sign in and choose **Account → Manage users…**. Enter a GitHub login and
   choose **Allow login**. Leave principal ID empty to create a new principal
   on first sign-in, or supply an existing Sift principal ID to link explicitly.
   GitHub's immutable numeric ID becomes the durable identity after sign-in;
   usernames are admission hints, and email never links accounts.
4. The admitted person connects their desktop to this server and chooses
   **Continue with GitHub** in Account. The button is enabled when the server
   reports configured GitHub sign-in. New users receive a personal tenant.
5. To grant team access, open **Account → Workspace invitations…**. Select a
   workspace you own/administer, choose **Viewer**, **Member**, or **Admin**,
   optionally enter the person's Sift principal ID, and create an invitation.
   Invitations expire in seven days. Copy the one-use token immediately; it is
   not saved, and closing Account clears it. An untargeted invitation can be
   accepted by any authenticated admitted user possessing the token.
6. The person signs in to the same server, opens **Workspace invitations…**,
   pastes the token, and chooses **Join workspace**. New membership appears
   immediately. The issuer can revoke unused invitations from that same view.
7. Add the member to intended shared rooms using **Room administration**.
   Invitations grant tenant membership only; connections, rooms, and roles
   remain separate permissions. Each user's Sift principal ID is in Account.

Account keyboard controls: `g` starts GitHub setup/sign-in, `i` opens
invitations, and `c` copies the current device code. In invitations, `h/l`
changes workspace, `r` cycles role, `j/k` selects an unused invitation, `d`
revokes it, `t` focuses the optional target, `p` focuses the join-token input,
and `y` copies the freshly issued token. Enter submits the focused field;
without input focus it creates an invitation. Tab cycles invitation fields.

The existing APIs remain available for automation:
`POST /v1/metadata/tenants/{id}/invitations`,
`DELETE /v1/metadata/tenants/{tenant_id}/invitations/{id}`, and
`POST /v1/auth/invitations/accept`. API callers can choose an expiry up to
30 days. Invitation revocation is fenced by the tenant authorized in the URL.

See [GitHub's OAuth device flow documentation](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow)
for registration requirements and authorization behavior.

## Set up Source Control

Source Control works on a **server-owned workspace projection**, not the
desktop's current checkout. The server operator must first enable workspaces
and Git and configure a logical workspace root handle. The root points to a
server filesystem directory; clients cannot enter arbitrary paths. For a
development server, see `SIFT_WORKSPACES__ROOTS`,
`SIFT_WORKSPACES__ENABLED`, and `SIFT_VCS__ENABLED` in `.env.example`.
Applied instances use the `[server.workspaces]` and `[server.vcs]` settings in
[instance configuration](INSTANCE-CONFIG.md#workspace-git-policy).

Open a workspace and choose **Set up repository…** in Source Control. Enter the
configured root handle, then choose **Bind existing**, **Initialize**, or
**Clone HTTPS**. Existing workspace files remain in place when initializing.
If a bound projection later loses its `.git` metadata, Source Control offers
**Initialize Git here**. That creates a new repository at the same root; it
cannot recover lost commit history. Restore the original `.git` directory from
backup if that history matters.

GitHub sign in authenticates to Sift. Fetching and pushing a GitHub repository
use separate credentials, scoped to each Sift principal. Add an HTTPS remote
and your PAT in **Remotes and credentials**. Fetch and push are explicit actions.
