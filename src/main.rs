//! GPU treemap viewer with a native macOS window.
mod history;
mod evidence;
mod evidence_input;
mod call_colors;
mod filter;
mod relationships;
mod syntax;
mod source_cache;
mod model;
mod scan;
#[cfg(target_os="macos")] mod platform;
#[cfg(target_os="macos")] mod canvas;
#[cfg(target_os="macos")] mod math;
#[cfg(target_os="macos")] mod map;
#[cfg(target_os="macos")] mod editor;
#[cfg(target_os="macos")] mod remote;
#[cfg(target_os="macos")] mod ui;

#[cfg(not(target_os="macos"))]
fn main() { eprintln!("The megamap window currently requires macOS. Use the scan binary for portable analysis."); std::process::exit(1); }

#[cfg(target_os="macos")]
fn main() {
    if let Err(error)=run() { eprintln!("megamap: {error}");std::process::exit(1); }
}

#[cfg(target_os="macos")]
fn run()->Result<(),String> {
    use std::{path::PathBuf,time::{Duration,Instant}};
    use platform::{Backend,Event,FrameStats};
    use remote::{Command,Remote};
    use serde_json::{json,Value};
    struct CaptureGuard;
    impl Drop for CaptureGuard { fn drop(&mut self) { evidence_input::cancel_capture(); } }
    let _guard=CaptureGuard;
    let mut root=None;
    let mut evidence=None;
    let mut remote_port=None;
    let mut size=(1400.0,900.0);
    let mut hidden=std::env::var_os("MEGAMAP_HIDE_WINDOWS").is_some();
    for arg in std::env::args().skip(1) {
        if arg=="--help" { println!("megamap [project] [--evidence=snapshot.json] [--remote[=port]] [--size=1400x900] [--hidden]");return Ok(()); }
        else if arg=="--remote" {remote_port=Some(None);}
        else if arg=="--hidden" {hidden=true;}
        else if let Some(value)=arg.strip_prefix("--remote=") {remote_port=Some(Some(value.parse::<u16>().map_err(|_|"Invalid remote port")?));}
        else if let Some(value)=arg.strip_prefix("--evidence=") {evidence=Some(PathBuf::from(value));}
        else if let Some(value)=arg.strip_prefix("--size=") {
            let (w,h)=value.split_once('x').ok_or("Use --size=WIDTHxHEIGHT")?;
            size=(w.parse::<f64>().map_err(|_|"Invalid width")?,h.parse::<f64>().map_err(|_|"Invalid height")?);
            if !size.0.is_finite()||!size.1.is_finite()||size.0<500.0||size.1<400.0||size.0>8192.0||size.1>8192.0 {return Err("Window size must be between 500x400 and 8192x8192".into());}
        } else if arg.starts_with('-') {return Err(format!("Unknown option: {arg}"));}
        else if root.replace(PathBuf::from(arg)).is_some() {return Err("Pass one project directory".into());}
    }
    let root=root.unwrap_or(std::env::current_dir().map_err(|e|e.to_string())?);
    if !root.is_dir() {return Err("Project directory does not exist".into());}
    let remote=remote_port.map(Remote::start).transpose()?;
    if let Some(remote)=&remote {eprintln!("megamap remote: http://{}",remote.address);}
    let mut backend=Backend::new("megamap",size.0,size.1,hidden)?;
    backend.set_title(&format!("megamap · {}",root.display()));
    let mut app=ui::App::new(root,evidence)?;
    let mut dirty=true;
    let mut stats=FrameStats::default();
    let mut frame_count=0u64;
    let mut app_cpu_ms=0.0;
    let mut quad_drops=0;
    let mut last_frame=Instant::now()-Duration::from_secs(1);
    let mut pending=Vec::new();
    let capture_dir=std::env::temp_dir().join(format!("spamton-megamap-{}",std::process::id()));
    while app.running {
        let active=app.map.needs_frame();
        let wait=if dirty { (15.0-last_frame.elapsed().as_secs_f64()*1000.0).clamp(0.0,15.0) }else if active {8.0}else{40.0};
        if let Some(event)=backend.poll(wait) {app.handle(event,&mut backend);dirty=true;}
        for _ in 0..255 {
            let Some(event)=backend.poll(0.0) else {break};
            app.handle(event,&mut backend);dirty=true;
        }
        if let Some(remote)=&remote {
            while let Ok(envelope)=remote.rx.try_recv() {
                match &envelope.command {
                    Command::Input(event)=>app.handle(event.clone(),&mut backend),
                    Command::Click{x,y,button,clicks,modifiers}=>{
                        app.handle(Event::PointerMove{x:*x,y:*y,modifiers:*modifiers},&mut backend);
                        app.handle(Event::PointerDown{x:*x,y:*y,button:*button,clicks:*clicks,modifiers:*modifiers},&mut backend);
                        app.handle(Event::PointerUp{x:*x,y:*y,button:*button,clicks:*clicks,modifiers:*modifiers},&mut backend);
                    }
                    _=>{}
                }
                pending.push(envelope);dirty=true;
            }
        }
        dirty|=app.tick();
        if app.map.loading()&&last_frame.elapsed()>=Duration::from_millis(16) {dirty=true;}
        if dirty&&(last_frame.elapsed()>=Duration::from_millis(15)||!pending.is_empty()) {
            let start=Instant::now();
            backend.begin_frame([0.035,0.045,0.055,1.0]);
            quad_drops=app.draw(&mut backend);
            stats=backend.present()?;
            app_cpu_ms=start.elapsed().as_secs_f64()*1000.0;
            if stats.skipped==0 {dirty=false;frame_count+=1;last_frame=Instant::now();}
        }
        if dirty {continue;}
        for envelope in pending.drain(..) {
            let reply:Value=match envelope.command {
                Command::Help=>json!({"app":"megamap","routes":["/s","/snap","/stats","/click","/m","/k","/t","/g","/gq","/quit"]}),
                Command::Snapshot{query,all}=>app.snapshot(query.as_deref(),all),
                Command::State=>app.state(),
                Command::Stats=>json!({"frames":frame_count,"app_cpu_ms":app_cpu_ms,"submit_cpu_ms":stats.cpu_ms,"gpu_ms":stats.gpu_ms,"instances":stats.instances,"draw_calls":stats.draw_calls,"blits":stats.blits,"atlas_glyphs":stats.atlas_glyphs,"atlas_used_bytes":stats.atlas_used_bytes,"atlas_capacity_bytes":platform::ATLAS_CAPACITY_BYTES,"atlas_upload_bytes":stats.atlas_upload_bytes,"dropped_instances":stats.dropped_instances,"dropped_glyphs":stats.dropped_glyphs,"dropped_quads":quad_drops,"skipped":stats.skipped}),
                Command::Grab{quit}=>{
                    std::fs::create_dir_all(&capture_dir).map_err(|e|e.to_string())?;
                    let path=capture_dir.join(format!("frame-{frame_count}.png"));
                    let result=match backend.capture(&path) {Ok(())=>json!({"png":[path]}),Err(error)=>json!({"error":error})};
                    if quit {app.running=false;}
                    result
                }
                Command::Quit=>{app.running=false;json!({"ok":true})}
                _=>json!({"ok":true}),
            };
            let _=envelope.reply.send(reply);
        }
    }
    Ok(())
}
