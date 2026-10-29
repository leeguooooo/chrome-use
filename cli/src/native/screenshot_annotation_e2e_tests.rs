//! Annotated captures must not add nodes or styles to the page.
use super::*;

#[tokio::test]
#[ignore]
async fn e2e_screenshot_projects_transformed_oop_frames() {
    let leaf = |name: &str| {
        format!(
        "<!doctype html><style>body{{margin:0;width:600px;height:700px}}button{{position:absolute;left:20px;top:30px;width:80px;height:40px;border:0}}</style><button>{name}</button>"
    )
    };
    let (leaf_port, leaf_server) = spawn_html_server(leaf("Specimen")).await;
    let nested_style = "position:absolute;left:30px;top:40px;width:160px;height:100px;border:0;transform-origin:0 0;transform:rotate(12deg) skewX(8deg)";
    let nested = format!("<!doctype html><style>body{{margin:0}}</style><iframe style='{nested_style}' src='http://127.0.0.1:{leaf_port}/'></iframe>");
    let (nested_port, nested_server) = spawn_html_server(nested).await;
    let mut state = DaemonState::new();
    for (index, transform) in [
        "scale(.8)",
        "rotate(25deg)",
        "skew(20deg,10deg)",
        "translateX(300px) scaleX(-1)",
        "perspective(450px) rotateY(30deg)",
        "rotate(-15deg) scale(.9)",
    ]
    .iter()
    .enumerate()
    {
        let control = if index == 5 {
            format!("<!doctype html><style>body{{margin:0}}</style><iframe style='{nested_style}' srcdoc='{}'></iframe>", leaf("Control"))
        } else {
            leaf("Control")
        };
        let cross_port = if index == 5 { nested_port } else { leaf_port };
        // Same-renderer control uses Chrome's own projection. The OOP copy
        // differs only by the outer slot's 500 CSS pixel horizontal offset.
        let html = format!(
            r##"<!doctype html><style>body{{margin:0;height:1400px}}.slot{{position:absolute;top:240px;left:100px}}.slot+.slot{{left:600px}}iframe{{width:300px;height:220px;border:0;transform-origin:0 0;transform:{transform}}}</style>
            <div class="slot"><iframe srcdoc="{}"></iframe></div>
            <div class="slot"><iframe src="http://localhost:{cross_port}/"></iframe></div>
            <script>window.mutations=[];new MutationObserver(r=>mutations.push(...r)).observe(document.documentElement,{{subtree:true,childList:true,attributes:true}});</script>"##,
            control.replace('&', "&amp;").replace('"', "&quot;")
        );
        let (port, server) = spawn_html_server(html).await;
        if index == 0 {
            launch_on(port, &mut state).await;
        } else {
            assert_success(&run_cmd(&mut state, json!({"id":"open","action":"navigate","url":format!("http://127.0.0.1:{port}/")})).await);
        }
        for scale in [1.0, 2.0] {
            assert_success(&run_cmd(&mut state, json!({"id":"viewport","action":"viewport","width":1100,"height":750,"deviceScaleFactor":scale})).await);
            eval_in(&mut state, "scrollTo(0,80);mutations=[]").await;
            let before = eval_in(&mut state, "Object.getOwnPropertyNames(window).sort()").await;
            let shot = run_cmd(
                &mut state,
                json!({"id":"shot","action":"screenshot","annotate":true,"base64":true}),
            )
            .await;
            assert_success(&shot);
            assert_eq!(eval_in(&mut state, "mutations.length").await, 0);
            assert_eq!(
                eval_in(&mut state, "Object.getOwnPropertyNames(window).sort()").await,
                before
            );
            let data = get_data(&shot);
            let labels = data["annotations"].as_array().unwrap();
            let control = labels
                .iter()
                .find(|a| a["name"] == "Control")
                .expect("same-renderer control");
            let specimen = labels
                .iter()
                .find(|a| a["name"] == "Specimen")
                .expect("OOP specimen");
            for key in ["x", "y", "width", "height"] {
                let expected =
                    control["box"][key].as_i64().unwrap() + if key == "x" { 500 } else { 0 };
                let actual = specimen["box"][key].as_i64().unwrap();
                assert!(
                    (actual - expected).abs() <= 1,
                    "{transform}, scale {scale}, {key}: expected {expected}, got {actual}"
                );
            }
            if let Some(path) = data["path"].as_str() {
                let _ = std::fs::remove_file(path);
            }
        }
        server.abort();
    }
    run_cmd(&mut state, json!({"id":"close","action":"close"})).await;
    leaf_server.abort();
    nested_server.abort();
}

#[tokio::test]
#[ignore]
async fn e2e_screenshot_annotations_leave_no_dom_mutations() {
    let child =
        "<!doctype html><style>button{width:90px;height:30px}</style><button>Child</button>";
    let (child_port, child_server) = spawn_html_server(child.to_string()).await;
    let html = r##"<!doctype html><meta charset="utf-8">
      <style>body{margin:0;height:1800px}button{position:absolute;left:30px;top:800px;width:90px;height:30px}</style>
      <button id="target">Target</button>
      <iframe style="position:absolute;left:200px;top:800px;width:160px;height:100px;border:3px solid" srcdoc="<!doctype html><style>button{width:90px;height:30px}</style><button>Child</button>"></iframe>
      <iframe style="position:absolute;left:400px;top:800px;width:160px;height:100px;border:3px solid" src="http://localhost:__CHILD_PORT__/"></iframe>
      <script>
      window.annotationMutations = [];
      new MutationObserver(records => window.annotationMutations.push(...records))
        .observe(document.documentElement, {subtree:true,childList:true,attributes:true});
      </script>"##;
    let (port, server) =
        spawn_html_server(html.replace("__CHILD_PORT__", &child_port.to_string())).await;
    let mut state = DaemonState::new();
    launch_on(port, &mut state).await;
    for scale in [1.0, 2.0] {
        let viewport = run_cmd(&mut state, json!({"id":"viewport","action":"viewport","width":1000,"height":700,"deviceScaleFactor":scale})).await;
        assert_success(&viewport);
        eval_in(&mut state, "window.scrollTo(0,600)").await;
        for (index, extra) in [
            json!({}),
            json!({"fullPage":true}),
            json!({"selector":"#target"}),
            json!({"clip":{"x":20,"y":790,"width":600,"height":200}}),
        ]
        .iter()
        .enumerate()
        {
            eval_in(&mut state, "window.annotationMutations=[]").await;
            let mut command = json!({"id":format!("shot-{index}"),"action":"screenshot","annotate":true,"base64":true});
            for (key, value) in extra.as_object().unwrap() {
                command[key] = value.clone();
            }
            let viewport_width = eval_in(&mut state, "visualViewport.width")
                .await
                .as_f64()
                .unwrap();
            let content_width = eval_in(&mut state, "document.documentElement.scrollWidth")
                .await
                .as_f64()
                .unwrap();
            let response = run_cmd(&mut state, command).await;
            assert_success(&response);
            assert_eq!(
                eval_in(&mut state, "window.annotationMutations.length").await,
                0
            );
            let data = get_data(&response);
            assert!(
                !data["annotations"].as_array().unwrap().is_empty(),
                "missing labels: {response}"
            );
            let annotations = data["annotations"].as_array().unwrap();
            let target = annotations
                .iter()
                .find(|a| a["name"] == "Target")
                .expect("target label");
            let (x, y) = [(30, 200), (30, 800), (0, 0), (10, 10)][index];
            assert_eq!(
                target["box"],
                json!({"x":x,"y":y,"width":90,"height":30}),
                "scale {scale}, capture {index}"
            );
            if index != 2 {
                let mut children: Vec<_> = annotations
                    .iter()
                    .filter(|a| a["name"] == "Child")
                    .map(|a| {
                        (
                            a["box"]["x"].as_i64().unwrap(),
                            a["box"]["y"].as_i64().unwrap(),
                        )
                    })
                    .collect();
                children.sort();
                let (dx, dy) = [(0, 600), (0, 0), (0, 0), (20, 790)][index];
                assert_eq!(
                    children,
                    [(211 - dx, 811 - dy), (411 - dx, 811 - dy)],
                    "scale {scale}, capture {index}"
                );
            }
            let pixels = STANDARD.decode(data["base64"].as_str().unwrap()).unwrap();
            let image = image::load_from_memory(&pixels).unwrap().to_rgb8();
            let css_width = [viewport_width, content_width, 90.0, 600.0][index];
            let raster_scale = image.width() as f64 / css_width;
            assert_eq!(raster_scale, scale, "capture scale {index}");
            assert_eq!(
                image
                    .get_pixel(
                        (x as f64 * raster_scale) as u32,
                        ((y as f64 + 15.0) * raster_scale) as u32
                    )
                    .0,
                [230, 32, 32],
                "label border at capture coordinates"
            );
            assert!(
                image.pixels().any(|p| p.0 == [230, 32, 32]),
                "no rendered labels"
            );
            if let Some(path) = data["path"].as_str() {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    run_cmd(&mut state, json!({"id":"close","action":"close"})).await;
    server.abort();
    child_server.abort();
}

/// A selector is measured in viewport coordinates but must crop document pixels.
/// This also exercises the ordinary (unannotated) off-screen capture path.
#[tokio::test]
#[ignore]
async fn e2e_screenshot_regions_preserve_pixels_after_scroll_without_annotations() {
    let html = r##"<!doctype html><style>
      body { margin:0;height:3200px;background:white }
      .block { position:absolute;left:40px;width:160px;height:80px }
      #visible { top:800px;background:rgb(18,171,52) }
      #offscreen { top:2400px;background:rgb(31,89,211) }
      </style><div class="block" id="visible"></div><div class="block" id="offscreen"></div>
      <script>window.captureMutations=[];new MutationObserver(r=>captureMutations.push(...r)).observe(document.documentElement,{subtree:true,childList:true,attributes:true});</script>"##;
    let (port, server) = spawn_html_server(html.to_string()).await;
    let mut state = DaemonState::new();
    launch_on(port, &mut state).await;
    for scale in [1.0, 2.0] {
        assert_success(&run_cmd(&mut state, json!({"id":"vp","action":"viewport","width":1000,"height":700,"deviceScaleFactor":scale})).await);
        eval_in(&mut state, "scrollTo(0,600);captureMutations=[]").await;
        let before = eval_in(&mut state, "Object.getOwnPropertyNames(window).sort()").await;
        for (index, extra, size, expected) in [
            (
                0,
                json!({"selector":"#visible"}),
                (160.0, 80.0),
                [18, 171, 52],
            ),
            (
                1,
                json!({"selector":"#offscreen"}),
                (160.0, 80.0),
                [31, 89, 211],
            ),
            (
                2,
                json!({"clip":{"x":60,"y":810,"width":80,"height":40}}),
                (80.0, 40.0),
                [18, 171, 52],
            ),
        ] {
            let mut cmd = json!({"id":format!("crop-{index}"),"action":"screenshot","base64":true});
            for (key, value) in extra.as_object().unwrap() {
                cmd[key] = value.clone();
            }
            let shot = run_cmd(&mut state, cmd).await;
            assert_success(&shot);
            let data = get_data(&shot);
            let path = data["path"].as_str().unwrap();
            let image = image::open(path).unwrap().to_rgb8();
            assert_eq!(
                image.dimensions(),
                ((size.0 * scale) as u32, (size.1 * scale) as u32),
                "{data}"
            );
            assert_eq!(
                image.get_pixel(image.width() / 2, image.height() / 2).0,
                expected,
                "wrong document region at scale {scale}, case {index}"
            );
            assert_eq!(eval_in(&mut state, "scrollY").await, json!(600));
            assert_eq!(
                eval_in(&mut state, "captureMutations.length").await,
                json!(0)
            );
            assert_eq!(
                eval_in(&mut state, "Object.getOwnPropertyNames(window).sort()").await,
                before
            );
            assert!(data
                .get("annotations")
                .is_none_or(|labels| labels.as_array().is_some_and(Vec::is_empty)));
            let _ = std::fs::remove_file(path);
        }
    }
    run_cmd(&mut state, json!({"id":"close","action":"close"})).await;
    server.abort();
}
