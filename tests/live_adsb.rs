use std::time::Instant;

use planeradar::adsb::{AdsbClient, AltitudeFilter};
use planeradar::http::UreqHttpClient;
use planeradar::model::Location;

#[test]
#[ignore = "contacts the live adsb.fi service with explicit test coordinates"]
fn live_tls_verified_adsb_request() {
    let latitude = std::env::var("PLANERADAR_TEST_LAT")
        .expect("PLANERADAR_TEST_LAT must be explicitly set")
        .parse()
        .expect("valid test latitude");
    let longitude = std::env::var("PLANERADAR_TEST_LON")
        .expect("PLANERADAR_TEST_LON must be explicitly set")
        .parse()
        .expect("valid test longitude");
    let location = Location {
        latitude,
        longitude,
        label: String::new(),
    };
    let client = AdsbClient::new(UreqHttpClient);
    let started = Instant::now();
    let aircraft = client
        .fetch(
            &location,
            13.3333,
            AltitudeFilter {
                minimum_feet: None,
                maximum_feet: None,
            },
        )
        .expect("TLS-verified ADS-B request");

    println!(
        "aircraft_count={} request_duration_ms={}",
        aircraft.len(),
        started.elapsed().as_millis()
    );
}
