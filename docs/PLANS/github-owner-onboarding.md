# GitHub owner onboarding and team admission

## Contract

GitHub sign-in already exists for hosted servers. Extend it with an explicit
local owner setup flow and desktop team invitations, preserving ADR-030's
closed registration and independent hosting.

- Personal loopback instances remain usable offline with their local identity.
  Only a verified trusted-local, active instance administrator can start or
  complete owner linking. A network visitor can never claim an instance.
- Local owner setup uses GitHub device authorization with an operator-configured
  OAuth App client ID and device flow enabled. No shared registration or client
  secret is bundled. Applied instances read the optional client ID from
  `auth.github.client_id` in local-device mode; other local servers read
  `SIFT_AUTH__GITHUB_DEVICE_CLIENT_ID` from their private `.env`.
- Each bounded, expiring attempt belongs to the initiating principal and daemon.
  Device codes stay in server memory; only a redacted, unguessable handoff and
  the human verification code reach the desktop. Polling honors GitHub's
  interval and slow-down responses and prevents concurrent upstream polls.
- Completion fetches GitHub's current authenticated profile, discards the token,
  and atomically links the immutable numeric subject to the existing local
  administrator. Existing or manifest-declared identities cannot be replaced.
  Disabled administrators and identities fail closed. Audit contains no codes
  or credentials. Local OS ownership remains the recovery route.
- Hosted/team servers keep authorization-code OAuth, state, PKCE, rotating Sift
  sessions, and explicit GitHub admission. Local linking does not expose a
  server to the network or change deployment policy. Hosting requires the
  existing reviewed instance configuration and per-instance OAuth registration.
- Account offers owner setup/verification, explains missing configuration, and
  shows the device code with copy and browser actions while waiting.
- Desktop team invitations expose existing create/list/revoke/accept APIs.
  Issuers select an administrated tenant, role, and optional target principal.
  One-use tokens are visible only after issuance and never stored in settings.
  Acceptance grants tenant membership only; rooms remain separate grants.
- Authentication and invitation UI resets sensitive transient state on dismissal,
  identity changes, and server switches. Delayed results are scoped to the
  initiating instance and principal.

## Milestones

1. Server device flow, atomic owner binding, SDK and security behavior tests.
2. Native owner setup and team invitation UI with Vim interaction and scoped
   asynchronous results.
3. Required workspace checks, operator/recovery documentation, and final ADR.

## Acceptance

Reject network/SSH/team owner claims, changed principals, disabled owners,
subject collisions and attempted identity replacement. Handle expiry, denial,
slow-down, cancellation, and one-use completion without storing GitHub tokens.
Verify profile synchronization and stable existing principal/tenant ownership.
Verify targeted invitations, role choice, revocation, one-use acceptance, and
stale UI suppression. No GitHub account or registration is created by tests;
bounded local provider fixtures verify protocol behavior.

Reference: [GitHub device authorization](https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow).
