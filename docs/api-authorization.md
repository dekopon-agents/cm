# API authorization contract

The API authorizes actions from a JWT signed by a configured trusted key. Names,
placement labels and job titles confer no permissions. There are no privileged
subjects, role claims, permission bundles or wildcard grants.

This is the contract for the planned API backend, not an authentication
implementation for the existing Git backend.

## Public machine-readable contract

- [JWT claims schema](../schemas/auth/jwt-claims.v1.schema.json)
- [Action scope catalog](../schemas/auth/scopes.v1.json)
- [Catalog schema](../schemas/auth/scope-catalog.v1.schema.json)

Each scope has a meaning; each operation declares its exact required scopes.
Clients can build capability-aware interfaces from this catalog. A hidden button
does not replace server authorization. Pin a public repository commit for builds.

The service will expose these release-bundled files without authentication at
`GET /auth/jwt.schema.json` and `GET /auth/scopes.json`. They contain no credentials
or campaign data. Until the service exists, use these public repository files.
The running service must expose the same bytes as its release contract.

## Verification and authorization

1. Use a maintained JWT library with configured algorithms and trusted issuer/keys.
   Never trust a token-selected key URL or algorithm. Validate exact issuer and
   audience, expiry, optional not-before, issued-at policy, revocation and the
   deployment minimum-issued-at cutoff. Signing keys stay with the signer.
2. Validate claims shape. JSON Schema does NOT verify signatures, compare times
   with the clock, or check revocations.
3. Split `scope` on ASCII spaces; compare complete case-sensitive words. Require
   every scope in an operation's `all_of`. Writes imply no reads. Unknown words
   grant nothing. There is no prefix, substring or wildcard expansion.
4. Apply an optional exact `campaign_id`/`assignment_id` restriction uniformly to every scope.
   A campaign-restricted token refuses instance-resource operations, even on a
   single-campaign deployment. Assignment constraints require qualified closure
   over every loaded object; unsupported operations deny. If absent, scopes cover this API instance. Equal scopes and constraints give
   equal authorization regardless of `sub` or `client_id`.
5. Check revision, idempotency, evidence, target and lifecycle preconditions.
   Permission cannot fabricate a native review, usage observation or deployment.

`sub` is an opaque attribution/deduplication identifier. `client_id` is an opaque
placement label, not environmental attestation or a grant. Distinct `jti` values
support independent revocation. Long-lived tokens remain supported with finite
expiry chosen at issuance.

Invalid authentication returns 401; a verified token lacking required scopes or
violating its resource restriction returns 403. Unmapped operations deny by
default. Only explicitly listed authentication=public operations need no token.
The one github_hmac event route instead verifies GitHub origin; it grants no
campaign action and treats the payload solely as a bounded collection hint.

JWT operations use an HTTP Bearer/JWT OpenAPI security scheme, with `x-required-scopes` generated
from the catalog. Do not invent an OAuth login/token endpoint to describe scopes.
API CI must compare actual handler operation IDs and scope declarations with the
catalog before qualification; these files alone do not prove handler enforcement.

A compound command requires the union of its typed operations’ scopes, checked
before one atomic admission. For example, raising a capped review and resolving
its escalation requires both `cm:reviews:raise-cap` and `cm:escalations:resolve`;
resolving alone cannot change the cap.

## Example claims

```json
{
  "iss": "https://issuer.example.test/cm",
  "aud": "campaign-trail-api",
  "sub": "principal-07",
  "jti": "credential-19",
  "iat": 1795737600,
  "exp": 1827273600,
  "client_id": "client-03",
  "scope": "cm:cards:read cm:escalations:resolve",
  "campaign_id": "campaign-11"
}
```

This is not a signed credential. It permits neither starting sessions nor
publishing releases. Resolving an escalation does not itself raise a round cap;
the accepted disposition must explicitly authorize that change.

The service uses scoped credentials for its own operations too, with no
controller-role bypass. Accepted authorization is recorded with operation intent.
Authenticate/authorize before dedup and old-result access. Store accepting jti
and scope/resource/decision bindings. Revocation blocks new admissions AND
accepted-but-unsent dispatch in the same SQL boundary; unknown/sent requests
remain accountable rather than being recalled or blindly relaunched. Provider credentials remain separate from API JWTs.

Initially dispatched workers/reviewers receive no API credential: packets go in
and native results come out by run ID. Native children never inherit a parent
credential. Selected command/judgement clients may use explicit action scopes;
placement labels confer no authority or environmental attestation. Constrained
delegation requires the issuer/ancestry/delivery qualification below before it
is enabled; it is not an excuse to forward a parent into a model environment.

## Maintenance

Scope meanings are versioned application interfaces. New actions need explicit
operation mappings. The catalog is release-bundled data checked against typed
handlers, not a remotely supplied policy program or a role registry.

```sh
python3 -m pip install -r tools/auth-contract-requirements.txt
python3 tools/check_auth_contract.py
```

## Lane, track and executor operations

Tracks describe work; executors describe qualified execution adapters; lanes pair
them. Queue edits require both lane-queue and Card-write scopes. Cross-track
reclassification additionally requires Track-write; draining also requires
Lane-write to pause source admissions. Active claims, uncertain sends, running
operations and unconfirmed stops prohibit movement regardless of scopes.
Configuration cannot manufacture healthy observations. Missing mandatory usage
meters pause admissions rather than imply unlimited budget. Routing changes use
the separate Track-route scope; ordinary metadata writes cannot change fallback
policy. Nested lane/handoff views require every constituent read scope listed in
the catalog. Artifact accountability belongs to a durable work track, not a
Session or an authorization role. These are contract requirements for the future
service, not claims of an installed scheduler or health loop.


## Authentication and resource classes

Each catalog operation declares authentication (`jwt`, `public`, `github_hmac`),
resource (`campaign`, `instance`, `public`) and exact all_of scopes. This replaces
the pre-freeze redundant public boolean; no compatibility alias or second policy
registry is added. JWT operations need scopes; public and GitHub-HMAC operations
have empty all_of. HMAC is NOT anonymous public access. Instance operations include
whole-database backup, credential revocation and recovery; campaign-restricted
credentials cannot call them. Campaign checks use server-loaded objects, including
all nested/bulk/source/destination references, never the body campaign label.

[Ingress inventory](../schemas/auth/ingress-routes.v1.json) enumerates public static/
health/schema routes and the HMAC endpoint. Handler/route inventory CI must match
the release catalog and build-manifest assets, denying unknown handlers. The
HMAC route verifies bounded raw body, exact configured installation/repository,
delivery dedup and rate/coalescing bounds before authoritative refetch. Payload
facts never certify a merge/run or clear a hold. JWT action scopes stay exclusive
for campaign commands; event authentication creates no action grant.

Executor health/meter/qualified capabilities are response-only native facts.
There is no public recordExecutorObservation operation. configureExecutor only
changes lifecycle subject to domain guards. Track/lane limit setters (including
creation with initial limits) require their write scope AND cm:campaigns:approve
with accepted exact values; metadata cannot smuggle limits/routing into a patch.
Nested briefs, task, driver and attention views require constituent read scopes.

## Constrained delegation (requires issuer qualification)

A bearer JWT is not a signing key. cm:credentials:delegate permits a trusted
issuer to mint a child only within the parent's exact known scopes, issuer/
audience, campaign/assignment constraints, expiry and approved key window. No
implicit delegation scope passes to the child. A root stays in a trusted non-model
consumer; only the narrow child is delivered to a model destination. The planned
identity-service mint capability must be qualified; no API pod signing Secret or
existent token endpoint is implied by these schemas.

Children carry parent_jti, with a canonical nonsecret registered ancestry graph.
Every ancestor must remain valid and unrevoked; parent revocation invalidates its
entire subtree. Enforce finite chain depth, unchanged/narrower resources, current
cutoff/key trust and dispatch-time revocation; never embed raw parent JWTs. An
assignment constraint refuses any operation lacking explicit qualified resource
closure. Campaign delegation cannot issue instance credentials. A campaign subtree
revoke cannot target unrelated/root credentials. Unknown mint replies reconcile
by issuance ID and fixed child jti; secret delivery is private, authorized and
separate from ordinary results/audit/backups. No token alone proves these checks
exist; actual mint/HTTP/restore tests are qualification work.


The catalog's assignment_bound flag declares the explicit projection/closure
contract: only getBrief (the accepted assignment packet), getSession and getEvidence
are initially declared. False means assignment-restricted credentials are refused,
including delegation itself. A true flag still needs actual HTTP/reducer isolation
qualification; it is not evidence that an unimplemented handler is safe.
