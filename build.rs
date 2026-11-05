use std::path::Path;
use std::{env, fs};

fn main() {
    // 把图标编进 exe; 资源 ID 为 1, GPUI 建窗口时按这个 ID 取窗口和任务栏图标.
    println!("cargo:rerun-if-changed=assets/qrun.ico");
    embed_resource::compile("assets/qrun.rc", embed_resource::NONE).manifest_optional().unwrap();

    // 字体随程序放在 exe 旁的 fonts 目录 (发布时由安装包和 zip 带上); 开发时复制到 target\<profile>\fonts.
    // OUT_DIR 是 target\<profile>\build\qrun-xxx\out, 往上三级就是 exe 所在目录.
    println!("cargo:rerun-if-changed=assets/fonts");
    let out_dir = env::var("OUT_DIR").unwrap();
    let fonts = Path::new(&out_dir).ancestors().nth(3).unwrap().join("fonts");
    fs::create_dir_all(&fonts).unwrap();
    for entry in fs::read_dir("assets/fonts").unwrap() {
        let path = entry.unwrap().path();
        fs::copy(&path, fonts.join(path.file_name().unwrap())).unwrap();
    }
}
