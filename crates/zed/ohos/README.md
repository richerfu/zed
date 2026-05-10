# Zed OHOS 接入说明（ability 入口）

本分支在 `crates/zed` 中完成了通过 OHOS `ability` 作为唯一启动入口的接入：

- `crates/zed/src/main.rs`
  - 增加 `create_application()`：在 OHOS 下优先注入 `OpenHarmonyApp`，在非 OHOS 下仍走 `gpui_platform::current_platform(false)`。
  - 暴露 `run_with_ability_entry(app: OpenHarmonyApp)`：仅负责记录 capability 入口后调用原始 `main()`。
- `crates/zed/src/lib.rs`
  - OHOS 下 `include!("main.rs")` 复用完整启动逻辑。
  - `#[ability] pub fn openharmony_app(app: OpenHarmonyApp)` 直接调用 `run_with_ability_entry(app)`，作为 OHOS 的入口。
- `crates/zed/Cargo.toml`
  - 为 `target_env = "ohos"` 配置 `openharmony-ability` / `openharmony-ability-derive` / `napi-ohos` / `napi-derive-ohos`。
  - 将 `gpui_platform` 在 OHOS 下移除 `x11/wayland` 依赖，避免非目标环境 feature 冲突。
  - `build-dependencies` 保留 `napi-build-ohos`。
- `crates/zed/build.rs`
  - 当 `CARGO_CFG_TARGET_ENV == "ohos"` 时执行 `napi_build_ohos::setup()`；原有 Linux/Windows/macOS 构建逻辑保持兼容。

## 1) 建议接入结构

- 外部 OHOS 工程引入 `zed` 为依赖后，不再需要额外的二次入口层。
- 直接使用 `openharmony-ability` 约定导出的 `openharmony_app` 函数作为鸿蒙 Ability 启动点。
- ArkTS 壳工程必须让 `moduleName` 和 native 库名一致：`libzed.so` 对应 `moduleName = "zed"`。
- 使用 `defaultPage` 时建议强制 `loadMode = "sync"`，避免默认页通过动态 `import("libzed.so")` 加载 native 模块时出现黑屏且无明显错误日志。

最小 `EntryAbility.ets`：

```ts
import 'libzed.so';
import { NativeAbility } from '@ohos-rs/ability';

export default class EntryAbility extends NativeAbility {
  public moduleName: string = 'zed';
  public defaultPage: boolean = true;
  public loadMode: 'sync' | 'async' = 'sync';
}
```

如果仍然需要自行接管 ArkUI 页面，关闭 `defaultPage` 后显式使用 `DefaultXComponent`：

```ts
import 'libzed.so';
import { NativeAbility } from '@ohos-rs/ability';

export default class EntryAbility extends NativeAbility {
  public moduleName: string = 'zed';
  public defaultPage: boolean = false;
  public loadMode: 'sync' | 'async' = 'sync';
}
```

```ts
import { DefaultXComponent } from '@ohos-rs/ability';

@Entry
@Component
struct Index {
  build() {
    DefaultXComponent({ moduleName: 'zed' })
      .width('100%')
      .height('100%');
  }
}
```

## 2) 本地编译检查

```bash
cd crates/zed
ohrs build --arch aarch --package zed
```

### 方案 B：按 gpui template 方式在独立 OHOS 工程中接入

1. 使用 `ohrs` 或你现有 OHOS 工程工作流创建应用壳。
2. 将 `zed` 作为库依赖接入（示例）：

```toml
[dependencies]
zed = { path = "../path/to/zed/crates/zed", default-features = false, features = ["test-support"] } # 按需关闭/保留 features
openharmony-ability = { git = "https://github.com/harmony-contrib/openharmony-ability.git" }
openharmony-ability-derive = { git = "https://github.com/harmony-contrib/openharmony-ability.git" }
napi-ohos = "1.1.6"
napi-derive-ohos = "1.1.6"

[build-dependencies]
napi-build-ohos = "1.1.6"
```

3. 在应用壳 `Cargo.toml` 中确保：

```toml
[lib]
crate-type = ["cdylib", "rlib"]
```

4. 复用本分支：
   - 直接使用 `crates/zed/src/lib.rs` 暴露的 `openharmony_app`
   - 并保持二进制入口 `crates/zed/src/main.rs` 不变（用于桌面端）
