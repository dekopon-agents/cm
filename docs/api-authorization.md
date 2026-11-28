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
4. Apply an optional exact `campaign_id` restriction uniformly to every scope.
   If absent, scopes cover this API instance. Equal scopes and constraints give
   equal authorization regardless of `sub` or `client_id`.
5. Check revision, idempotency, evidence, target and lifecycle preconditions.
   Permission cannot fabricate a native review, usage observation or deployment.

`sub` is an opaque attribution/deduplication identifier. `client_id` is an opaque
placement label, not environmental attestation or a grant. Distinct `jti` values
support independent revocation. Long-lived tokens remain supported with finite
expiry chosen at issuance.

Invalid authentication returns 401; a verified token lacking required scopes or
violating its resource restriction returns 403. Unmapped operations deny by
default. Only explicitly listed public operations need no token.

Use an HTTP Bearer/JWT OpenAPI security scheme, with `x-required-scopes` generated
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
Revocation prevents new admissions without pretending to recall accepted remote
requests. Provider credentials remain separate from API JWTs.

Model execution environments receive only the read scopes their assignments need.
Environments provisioned with mutation/decision scopes are excluded from worker/
reviewer dispatch. Native children explicitly select execution environments and
never inherit a parent credential. The API cannot observe all out-of-band spawns.

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
