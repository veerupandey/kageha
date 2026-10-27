use kageha_browser_decisions::typed::*;
use serde_json::json;
use std::collections::BTreeMap;
fn request() -> Request {
    Request {
        state: json!({"observed":"synthetic"}),
        questions: BTreeMap::from([(
            "q".into(),
            Question {
                instructions: "Is the supplied condition explicit?".into(),
                criteria: BTreeMap::from([
                    ("true".into(), "Supported".into()),
                    ("false".into(), "False or unknown".into()),
                ]),
            },
        )]),
    }
}
#[test]
fn actual_distribution_not_generated_confidence_controls_acceptance() {
    let r = request();
    let result=Result::parse(&r,&json!({"success":true,"result":{"answers":{"q":{"type":"choice","choice":"true","confidence":1,"probabilities":{"true":0.61,"false":0.39}}}}})).unwrap();
    assert_eq!(result.answers["q"].probability(), 0.61);
}
#[test]
fn malformed_distributions_and_coverage_are_rejected() {
    let r = request();
    for answers in [
        json!({}),
        json!({"q":{"type":"choice","choice":"true","probabilities":{"true":0.1,"false":0.9}}}),
        json!({"q":{"type":"choice","choice":"true","probabilities":{"true":0.9,"unknown":0.1}}}),
        json!({"q":{"type":"choice","choice":"true","probabilities":{"true":0.9,"false":0.9}}}),
        json!({"q":{"type":"choice","choice":"true","probabilities":{"true":1.1,"false":-0.1}}}),
    ] {
        assert!(Result::parse(&r, &json!({"answers":answers})).is_err());
    }
}
#[test]
fn question_and_choice_limits_are_enforced_before_io() {
    let mut r = request();
    r.questions.clear();
    assert!(r.validate().is_err());
    for i in 0..65 {
        r.questions
            .insert(format!("q{i}"), request().questions["q"].clone());
    }
    assert!(r.validate().is_err());
}

#[test]
fn provider_confidence_is_retained_and_invalid_scores_are_rejected() {
    let r = request();
    let raw = json!({"answers":{"q":{"type":"choice","choice":"true","confidence":0.4,"probabilities":{"true":0.99,"false":0.01}}}});
    assert_eq!(
        Result::parse(&r, &raw).unwrap().answers["q"].confidence,
        Some(0.4)
    );
    for invalid in [json!(-0.01), json!(1.01), json!("0.99")] {
        let mut body = raw.clone();
        body["answers"]["q"]["confidence"] = invalid;
        assert!(Result::parse(&r, &body).is_err());
    }
    let mut result = Result::parse(&r, &raw).unwrap();
    result.answers.get_mut("q").unwrap().confidence = Some(f64::NAN);
    assert!(result.validate(&r).is_err());
}
