use monoize::accounting::money::{
    Currency, ExchangeRateRational, MoneyError, NanoMoney, convert_nano,
};

#[test]
fn signed_conversion_rounds_half_ties_away_from_zero_in_both_directions() {
    for (amount, source, target, rate, expected) in [
        (1, Currency::USD, Currency::CNY, "0.5", 1),
        (-1, Currency::USD, Currency::CNY, "0.5", -1),
        (3, Currency::USD, Currency::CNY, "0.5", 2),
        (-3, Currency::USD, Currency::CNY, "0.5", -2),
        (1, Currency::CNY, Currency::USD, "2", 1),
        (-1, Currency::CNY, Currency::USD, "2", -1),
        (3, Currency::CNY, Currency::USD, "2", 2),
        (-3, Currency::CNY, Currency::USD, "2", -2),
        (1, Currency::USD, Currency::CNY, "0.49", 0),
        (-1, Currency::USD, Currency::CNY, "0.49", 0),
        (1, Currency::USD, Currency::CNY, "0.51", 1),
        (-1, Currency::USD, Currency::CNY, "0.51", -1),
    ] {
        let rate = ExchangeRateRational::parse(rate).unwrap();
        assert_eq!(convert_nano(amount, source, target, &rate), Ok(expected));
    }
}

#[test]
fn both_i128_bounds_survive_unit_rate_and_same_currency_conversion() {
    let one = ExchangeRateRational::parse("1").unwrap();
    let twenty = ExchangeRateRational::parse("20").unwrap();
    for amount in [i128::MIN, -1, 0, 1, i128::MAX] {
        assert_eq!(
            convert_nano(amount, Currency::USD, Currency::CNY, &one),
            Ok(amount)
        );
        assert_eq!(
            convert_nano(amount, Currency::CNY, Currency::USD, &one),
            Ok(amount)
        );
        assert_eq!(
            convert_nano(amount, Currency::CNY, Currency::CNY, &twenty),
            Ok(amount)
        );
        assert_eq!(
            convert_nano(amount, Currency::USD, Currency::USD, &twenty),
            Ok(amount)
        );
    }
}

#[test]
fn widened_products_preserve_representable_results_at_both_bounds() {
    let rate = ExchangeRateRational::parse("1.5").unwrap();
    assert_eq!(
        convert_nano(i128::MAX, Currency::CNY, Currency::USD, &rate),
        Ok(113_427_455_640_312_821_154_458_202_477_256_070_485),
    );
    assert_eq!(
        convert_nano(i128::MIN, Currency::CNY, Currency::USD, &rate),
        Ok(-113_427_455_640_312_821_154_458_202_477_256_070_485),
    );
    let half = ExchangeRateRational::parse("0.5").unwrap();
    assert_eq!(
        convert_nano(i128::MIN, Currency::USD, Currency::CNY, &half),
        Ok(-85_070_591_730_234_615_865_843_651_857_942_052_864),
    );
}

#[test]
fn an_unrepresentable_final_amount_is_an_explicit_overflow() {
    let rate = ExchangeRateRational::parse("2").unwrap();
    for amount in [i128::MIN, i128::MAX] {
        assert_eq!(
            convert_nano(amount, Currency::USD, Currency::CNY, &rate),
            Err(MoneyError::AmountOverflow),
        );
    }
    let half = ExchangeRateRational::parse("0.5").unwrap();
    assert_eq!(
        convert_nano(i128::MAX, Currency::CNY, Currency::USD, &half),
        Err(MoneyError::AmountOverflow),
    );
}

#[test]
fn an_explicit_saved_rate_is_stable_after_a_live_rate_changes() {
    let saved = ExchangeRateRational::parse("6.7370").unwrap();
    let updated = ExchangeRateRational::parse("7.25").unwrap();
    assert_eq!(
        convert_nano(1_000_000_000, Currency::USD, Currency::CNY, &updated),
        Ok(7_250_000_000),
    );
    assert_eq!(
        convert_nano(1_000_000_000, Currency::USD, Currency::CNY, &saved),
        Ok(6_737_000_000),
    );
    assert_eq!(
        convert_nano(6_737_000_000, Currency::CNY, Currency::USD, &saved),
        Ok(1_000_000_000),
    );
}

#[test]
fn invalid_rates_cannot_enter_conversion() {
    for value in [
        "",
        "0",
        "-1",
        "+1",
        ".5",
        "1.",
        "01",
        "1e2",
        "NaN",
        "1.1234567890123456789",
    ] {
        assert!(
            ExchangeRateRational::parse(value).is_err(),
            "accepted {value}"
        );
    }
}

#[test]
fn a_money_envelope_rejects_a_currency_that_contradicts_its_epoch() {
    for (currency, epoch) in [
        (Currency::CNY, 0),
        (Currency::USD, 1),
        (Currency::USD, 2),
        (Currency::CNY, u64::MAX),
    ] {
        assert!(NanoMoney::new(1, currency, epoch).is_err());
        let invalid = NanoMoney {
            amount: 1,
            currency,
            epoch,
        };
        assert!(invalid.validate().is_err());
        assert!(serde_json::to_string(&invalid).is_err());
    }
}

#[test]
fn arithmetic_rejects_mixed_epochs_without_implicit_conversion() {
    let legacy = NanoMoney::new(10, Currency::USD, 0).unwrap();
    let current = NanoMoney::new(10, Currency::CNY, 1).unwrap();
    assert!(legacy.checked_add(current).is_err());
    assert!(current.checked_sub(legacy).is_err());
}

#[test]
fn arithmetic_is_checked_and_preserves_a_matching_denomination() {
    let debt = NanoMoney::new(-9, Currency::CNY, 1).unwrap();
    let credit = NanoMoney::new(4, Currency::CNY, 1).unwrap();
    assert_eq!(
        debt.checked_add(credit).unwrap(),
        NanoMoney::new(-5, Currency::CNY, 1).unwrap()
    );
    assert_eq!(
        debt.checked_sub(credit).unwrap(),
        NanoMoney::new(-13, Currency::CNY, 1).unwrap()
    );
    let one = NanoMoney::new(1, Currency::CNY, 1).unwrap();
    assert_eq!(
        NanoMoney::new(i128::MAX, Currency::CNY, 1)
            .unwrap()
            .checked_add(one),
        Err(MoneyError::AmountOverflow)
    );
    assert_eq!(
        NanoMoney::new(i128::MIN, Currency::CNY, 1)
            .unwrap()
            .checked_sub(one),
        Err(MoneyError::AmountOverflow)
    );
}

#[test]
fn explicit_conversion_validates_and_changes_the_whole_envelope() {
    let legacy = NanoMoney::new(-1, Currency::USD, 0).unwrap();
    let rate = ExchangeRateRational::parse("0.5").unwrap();
    assert_eq!(
        legacy.convert_to(Currency::CNY, 1, &rate).unwrap(),
        NanoMoney::new(-1, Currency::CNY, 1).unwrap()
    );
    assert!(legacy.convert_to(Currency::CNY, 0, &rate).is_err());
    let invalid = NanoMoney {
        amount: 1,
        currency: Currency::USD,
        epoch: 1,
    };
    assert!(invalid.convert_to(Currency::CNY, 1, &rate).is_err());
    assert!(invalid.checked_add(legacy).is_err());
}

#[test]
fn serialized_money_keeps_full_i128_precision_and_required_denomination() {
    for (amount, currency, epoch) in [(i128::MIN, Currency::USD, 0), (i128::MAX, Currency::CNY, 1)]
    {
        let money = NanoMoney::new(amount, currency, epoch).unwrap();
        let json = serde_json::to_value(money).unwrap();
        assert_eq!(json["amount"], amount.to_string());
        assert_eq!(
            json["currency"],
            if currency == Currency::USD {
                "USD"
            } else {
                "CNY"
            }
        );
        assert_eq!(json["epoch"], epoch);
        assert_eq!(serde_json::from_value::<NanoMoney>(json).unwrap(), money);
    }
}

#[test]
fn decoding_rejects_ambiguous_amounts_and_missing_or_conflicting_denomination() {
    for raw in [
        r#"{"amount":"1","epoch":1}"#,
        r#"{"amount":"1","currency":"CNY"}"#,
        r#"{"amount":"1","currency":"USD","epoch":1}"#,
        r#"{"amount":"1","currency":"CNY","epoch":2}"#,
        r#"{"amount":1,"currency":"CNY","epoch":1}"#,
        r#"{"amount":"+1","currency":"CNY","epoch":1}"#,
        r#"{"amount":"-0","currency":"CNY","epoch":1}"#,
        r#"{"amount":"00","currency":"CNY","epoch":1}"#,
        r#"{"amount":" 1","currency":"CNY","epoch":1}"#,
        r#"{"amount":"1.0","currency":"CNY","epoch":1}"#,
        r#"{"amount":"170141183460469231731687303715884105728","currency":"CNY","epoch":1}"#,
    ] {
        assert!(
            serde_json::from_str::<NanoMoney>(raw).is_err(),
            "accepted {raw}"
        );
    }
}
