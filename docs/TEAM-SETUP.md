# Team sign in and Source Control

## Add people to a server

Sift has its own principals. A GitHub account becomes a Sift principal only
after an instance administrator admits that GitHub login. GitHub sign in does
not grant access to an existing team room by itself.

1. Bootstrap the first administrator on a non-instance development server with
   `sift-admin bootstrap-admin <username> --password-stdin`. For an applied
   instance, use the bootstrap identity and claim flow in
   [instance configuration](INSTANCE-CONFIG.md).
2. Sign in to the hosted server from the desktop Account dialog. An instance
   administrator can choose **Manage users…** there.
3. In **Users**, create a password user, or enter a GitHub login and choose
   **Allow login**. A new GitHub user chooses **Continue with GitHub** in the
   Account dialog. Their first successful sign in creates their Sift principal.
   To link GitHub to an existing Sift principal, enter its ID when allowing
   the login. Sift never links accounts by email.
4. Grant access to a team tenant and its rooms separately. GitHub admission
   gives the person a personal tenant; it does not add team membership. Use the
   tenant invitation flow, then add the member to the intended shared room.
   The API is `POST /v1/metadata/tenants/{id}/invitations` with `role`,
   `target_principal_id` (optional), and an `expires_at` within 30 days.
   Give the returned one-use token to the person; they accept through
   `POST /v1/auth/invitations/accept`. Then use **Room administration** to add
   their principal ID to a shared room. Each person can see their principal ID
   in the Account dialog.

GitHub sign in requires a GitHub OAuth App for the server. Set the app's
callback URL to `<public HTTPS origin>/v1/auth/github/callback`, and configure
`SIFT_AUTH__PUBLIC_BASE_URL`, `SIFT_AUTH__GITHUB_CLIENT_ID`, and
`SIFT_AUTH__GITHUB_CLIENT_SECRET` in the server's private `.env`. Applied
instances use the OAuth credential slot described in
[instance configuration](INSTANCE-CONFIG.md). A GitHub username is used only
for admission; Sift records GitHub's immutable user ID after sign in. Sift
does not link a GitHub account to an existing password principal by email.

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
