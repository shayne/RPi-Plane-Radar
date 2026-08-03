# Route Confidence for ADSBDB Enrichment

Status: direction approved; written specification awaiting review

Date: 2026-08-02

Target repository: `shayne/RPi-Plane-Radar`

## Problem

Plane Radar currently treats the origin and destination returned by ADSBDB as
the current flight route. ADSBDB instead maps a callsign to a mostly static
route record. Its maintainer states that route data is updated roughly once a
year, and its issue tracker contains current examples where a valid live
callsign resolves to an obsolete route.

The application sends the Mode S identifier and callsign but does not send the
aircraft position, observation time, or current-flight identity to ADSBDB. It
therefore cannot ask ADSBDB which flight leg is currently operating. A static
route such as `IAH→ABQ` can be returned for an aircraft currently flying near
New York.

The local six-hour success cache is not the source of this mismatch, but its
current string-only representation would amplify it if a route were accepted
once and reused without considering the next aircraft position. ADSBDB also
supports an optional midpoint that Plane Radar currently ignores, which makes
a multi-leg itinerary appear to be a direct origin-to-destination flight.

## Goals

- Suppress ADSBDB routes that are clearly incompatible with the live aircraft
  position.
- Preserve plausible route labels and all current aircraft-model enrichment.
- Re-evaluate geographical plausibility for every live aircraft position even
  when the underlying ADSBDB route candidate is cached.
- Render ADSBDB's optional midpoint explicitly so a multi-leg itinerary is not
  presented as a direct flight.
- Fail closed when route codes or airport coordinates are missing, malformed,
  or non-finite.
- Keep route enrichment optional, asynchronous, bounded, and independent of
  the primary ADS-B refresh cadence.

## Non-goals

- Guaranteeing that ADSBDB identifies the current scheduled flight or its
  direction correctly.
- Inferring a current leg from aircraft heading, which can be misleading during
  vectors, holds, diversions, and approaches.
- Adding a paid provider, API key, account, or new privacy boundary.
- Replacing ADSBDB for aircraft-model enrichment.
- Persisting enrichment caches across process restarts.
- Changing radar settings, display defaults, tag order, or typography.
- Changing the ADS-B position provider.

## Considered approaches

### 1. Current-flight provider

A schedule or tracking provider keyed by a unique flight instance would give
the strongest route semantics. It would also introduce an API key, a new
privacy and availability boundary, possible usage charges, and more operational
configuration. This remains the path for authoritative routes, but it is not
part of this correction.

### 2. Position-aware confidence gate over ADSBDB

This is the approved approach. Plane Radar continues to use ADSBDB as a source
of route candidates, but requires a candidate's airport geometry to be
compatible with the aircraft's current position before displaying it. The
approach catches egregious mismatches without claiming that static route data
has become real-time data.

### 3. Label every route as estimated

Changing copy or adding a question mark would communicate uncertainty but
would still place geographically impossible information on the radar. It does
not satisfy the accuracy goal by itself.

## Provider response model

`FlightRoute` gains the optional ADSBDB `midpoint` field. Every origin,
midpoint, and destination endpoint includes:

- a preferred IATA code;
- an ICAO fallback code;
- latitude; and
- longitude.

Endpoint parsing accepts only a three-character ASCII-alphanumeric IATA code or
a four-character ASCII-alphanumeric ICAO code. Codes are normalized to upper
case. Coordinates must be finite, latitude must be between -90 and 90 degrees
inclusive, and longitude must be between -180 and 180 degrees inclusive.

An absent midpoint produces `ORIGIN→DESTINATION`. A valid midpoint produces
`ORIGIN→MIDPOINT→DESTINATION`. If a midpoint object is present but incomplete
or invalid, the entire route candidate is rejected rather than silently
misrepresenting the itinerary as direct.

The deserialized response keeps the `flightroute` field as an isolated JSON
value and validates it separately from the aircraft object. A malformed route
therefore becomes a route miss without discarding a valid model returned by the
same combined response.

## Structured route candidates

Provider parsing produces an internal `RouteCandidate`, not a display string.
The candidate contains the normalized label and two or three validated airport
coordinates. `FlightLookup.route` carries `LookupValue<RouteCandidate>` while
the renderer-facing `AircraftEnrichment.route` remains `Option<String>`.

The route cache stores `RouteCandidate` values by normalized callsign. Model
entries continue to store compact model strings by normalized Mode S hex.
Generic or separately typed cache entries keep these two value types explicit.

The existing time-to-live policy remains unchanged:

- successful provider candidates remain cached for six hours;
- definite provider misses remain cached for ten minutes; and
- each cache retains bounded least-recently-used eviction.

A cached candidate is not equivalent to an approved display route. Every cache
resolution evaluates the current aircraft latitude and longitude against the
candidate geometry. This means two aircraft using the same callsign at
different places can receive different display outcomes from one cached
candidate, and a moving aircraft can become plausible or implausible without a
new provider request.

## Geographical confidence rule

Route confidence uses great-circle geometry on the existing mean Earth radius.
For each origin-to-destination or origin-to-midpoint-to-destination segment:

1. calculate the shortest great-circle distance from the live aircraft point
   to the bounded segment, using endpoint distance when the perpendicular
   projection falls outside the segment;
2. calculate a deliberately generous corridor width as 20 percent of the
   segment length;
3. clamp that corridor to a minimum of 200 km and a maximum of 500 km; and
4. accept the route if the aircraft lies inside at least one segment corridor.

The minimum accommodates terminal vectors and short routes. The maximum allows
substantial airway and weather deviation on long flights while still rejecting
continental-scale mismatches such as an `IAH→ABQ` candidate over Manhattan.

Great-circle calculations normalize longitude differences across the
international date line. Segments shorter than 1 km or within `0.000001`
radians of an exact antipodal separation, invalid intermediate calculations,
and invalid live aircraft coordinates reject the candidate. The confidence
function does not use the radar location and does not send any additional data
to ADSBDB.

## Runtime behavior

The enrichment worker and lookup cadence remain unchanged. A provider response
with a valid candidate is recorded in the route cache whether or not that
candidate is plausible at the aircraft's current position. Cache resolution
then behaves as follows:

- plausible candidate: publish its compact route label;
- implausible candidate: publish no route and do not reserve a blank radar tag
  line;
- cached provider miss: publish no route; and
- model enrichment: behave exactly as before.

The cache still reports a route candidate as resolved after a successful
provider response, even when the current position suppresses its label. This
prevents repeated ADSBDB calls for a known static mismatch while allowing every
new position to re-evaluate the candidate.

Existing runtime identity checks remain authoritative. Enrichment is still
published only when the exact Mode S identifier and raw flight callsign remain
visible, and departed-aircraft enrichment is still pruned. This confidence
change does not weaken late-response protection.

## Failure handling

Malformed route data is treated as a definite route miss for the current
provider lookup. It does not fail an otherwise valid combined aircraft-model
response. HTTP, TLS, response-size, JSON, and top-level schema failures retain
their existing error and backoff behavior.

No new user-visible error is introduced. Route enrichment remains best effort:
the callsign, model, altitude, and live aircraft position continue to render
when a route is absent or suppressed.

## Testing

Focused unit and integration tests cover:

- a point on a normal two-airport route is accepted;
- Manhattan is rejected for an `IAH→ABQ` candidate;
- a candidate is accepted near either segment of a valid midpoint itinerary;
- midpoint labels include all three airport codes;
- a present but invalid midpoint rejects the candidate;
- invalid codes, missing coordinates, non-finite coordinates, and out-of-range
  coordinates reject the candidate;
- great-circle segment distance behaves across the international date line;
- points beyond a route endpoint use endpoint distance rather than an infinite
  great-circle line;
- the 200 km minimum and 500 km maximum corridor bounds are exact;
- one cached callsign candidate is independently evaluated for aircraft at a
  plausible and an implausible position;
- a moving aircraft reuses the provider candidate but updates the published
  route outcome without another lookup;
- model-only and route-disabled lookups remain unchanged at the public
  enrichment boundary;
- combined responses can still return a model when their route is malformed or
  geographically suppressed; and
- existing cache expiry, LRU eviction, worker identity, renderer, and request
  cadence tests continue to pass.

The full repository verification command remains `mise run verify`.

## Accuracy boundary

This design provides a conservative confidence filter, not authoritative flight
tracking. A stale ADSBDB route that happens to pass near the live aircraft can
still be displayed, and position alone cannot reliably distinguish two
directions along the same city pair. Plane Radar must use a current-flight
provider if authoritative origin, destination, direction, or operational leg
information becomes a product requirement.

## References

- [ADSBDB API and route schema](https://github.com/mrjackwills/adsbdb#readme)
- [ADSBDB maintainer explanation of static annual route data](https://github.com/mrjackwills/adsbdb/issues/77)
- [ADSBDB discussion of changing and multi-leg callsigns](https://github.com/mrjackwills/adsbdb/issues/17)
- [Current ADSBDB wrong-route reports](https://github.com/mrjackwills/adsbdb/issues/83)
