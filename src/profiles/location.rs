//! Only cell centres reach persistence: repeated distance/filter queries cannot
//! recover a device coordinate inside its cell. This is not location attestation.

pub(crate) const LOCATION_CHANGE_INTERVAL_SECONDS: i64 = 300;
const LATITUDE_CELL_DEGREES: f64 = 0.05;
const MIN_LONGITUDE_CELL_KM: f64 = 5.0;
const EARTH_CIRCUMFERENCE_KM: f64 = 40_030.173_592;

pub(crate) fn coarse_coordinates(latitude: f64, longitude: f64) -> (f64, f64) {
    let band = ((latitude + 90.0) / LATITUDE_CELL_DEGREES)
        .floor()
        .clamp(0.0, 3599.0);
    let latitude = -90.0 + (band + 0.5) * LATITUDE_CELL_DEGREES;
    // A single cell at each polar cap avoids very narrow longitude wedges.
    let longitude = if band == 0.0 || band == 3599.0 {
        0.0
    } else {
        let cells = (EARTH_CIRCUMFERENCE_KM * latitude.to_radians().cos() / MIN_LONGITUDE_CELL_KM)
            .floor()
            .max(1.0);
        let width = 360.0 / cells;
        let longitude = (longitude + 180.0).rem_euclid(360.0);
        -180.0 + ((longitude / width).floor() + 0.5) * width
    };
    // Match numeric(9,6) storage so repeated normalisation stays idempotent.
    let round = |value: f64| (value * 1_000_000.0).round() / 1_000_000.0;
    (round(latitude), round(longitude))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearby_points_share_a_stable_cell_without_preserving_device_coordinates() {
        let centre = coarse_coordinates(48.856613, 2.352222);
        assert_ne!(centre, (48.856613, 2.352222));
        assert_eq!(
            centre,
            coarse_coordinates(centre.0 + 0.001, centre.1 + 0.001)
        );
        assert_eq!(
            centre,
            coarse_coordinates(centre.0 - 0.001, centre.1 - 0.001)
        );
    }

    #[test]
    fn cells_are_valid_and_idempotent_across_latitudes_and_the_dateline() {
        for latitude in [
            -90.0, -89.99, -80.0, -0.001, 0.0, 48.856613, 80.0, 89.99, 90.0,
        ] {
            for longitude in [-180.0, -179.999, -2.352222, 0.0, 2.352222, 179.999, 180.0] {
                let centre = coarse_coordinates(latitude, longitude);
                assert!((-90.0..=90.0).contains(&centre.0));
                assert!((-180.0..180.0).contains(&centre.1));
                assert_eq!(centre, coarse_coordinates(centre.0, centre.1));
            }
            assert_eq!(
                coarse_coordinates(latitude, -180.0),
                coarse_coordinates(latitude, 180.0)
            );
        }
        assert_eq!(
            coarse_coordinates(90.0, 20.0),
            coarse_coordinates(90.0, -160.0)
        );
    }
}
