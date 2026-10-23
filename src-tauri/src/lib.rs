use std::fs::File;
use std::io::Write;
use std::process::Command;

// launch 运行外部启动器（如 Prism Launcher），返回标准输出。
#[tauri::command]
fn launch(program: String, args: Vec<String>) -> Result<String, String> {
    let output = Command::new(&program)
        .args(&args)
        .output()
        .map_err(|e| format!("failed to launch {program}: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

// download_file 下载文件到当前目录（.mrpack 等）。
#[tauri::command]
fn download_file(url: String, dest: String) -> Result<(), String> {
    let resp = reqwest::blocking::get(&url).map_err(|e| format!("download failed: {e}"))?;
    let bytes = resp.bytes().map_err(|e| format!("read failed: {e}"))?;
    let mut f = File::create(&dest).map_err(|e| format!("create {dest}: {e}"))?;
    f.write_all(&bytes).map_err(|e| format!("write {dest}: {e}"))?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![launch, download_file])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
