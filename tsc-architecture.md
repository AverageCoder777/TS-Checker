# Train Simulator Classic (TSC / RailWorks): Technical Specification for TS Checker

This document is the primary architectural and parsing specification for **TS Checker** (a Rust-based replacement for RW_Tools / TS-Tools). It details the directory layout, binary/XML structures, exact XML tags produced by `serz.exe`, dependency resolution algorithms (`BIN -> GeoPcDx / XSec -> TgPcDx`), scenario consist inspection, and asset replacement mechanics.

*(Note: Audio, Lua scripts, and animations are intentionally excluded from this specification).*

---

## 1. Core File System & Virtual File System (VFS) Rules

### 1.1. Root Namespace (`Provider` / `Product`)
All game assets and route files resolve relative to the `RailWorks\Assets\` and `RailWorks\Content\Routes\` directories.
* **Asset Physical Path:** `RailWorks\Assets\<Provider>\<Product>\<LocalBlueprintID>`
* **Route Physical Path:** `RailWorks\Contend\Routes\<Route-GUID>\`
* **Case-Insensitivity:** Windows paths and internal TSC strings have mixed casing. All path lookup tables in Rust must normalize strings (convert to lowercase and normalize `/` to `\`).

### 1.2. Handling `.ap` Archives (Asset Packs)
Many official DLCs pack their `Assets\<Provider>\<Product>\` or `Content\Routes\<Route-GUID>\` folders into `.ap` files.
* **Format:** Standard ZIP archives (usually stored with `STORE` / 0-compression, though `DEFLATE` can occur).
* **VFS Priority Rule (Loose Files vs `.ap`):**
  1. If a loose file exists on disk (e.g., `Assets\DTG\Academy\...\file.bin`), the game loads the loose file.
  2. If the loose file does NOT exist on disk, the game checks inside any `.ap` archive located in that `Product` or `Route` folder.
  3. **TS Checker Requirement:** Build an in-memory index of both physical files and `.ap` archive central directories (`zip::ZipArchive::file_names()`) before checking dependencies. Do not unpack entire `.ap` archives to disk unless modifying a file inside them.

### 1.3. The `.xml` to `.bin` Quirk (CRITICAL)
Inside any TSC file (whether in `Tracks.bin`, `Scenery` tiles, `Scenario.bin`, or another `.bin` blueprint), **all references to blueprints end with the `.xml` extension**, even though the actual file on disk is compiled as `.bin`!
* **Internal String:** `RailVehicles\Diesel\Class47\Default\Engine\class47.xml`
* **Actual File on Disk:** `RailVehicles\Diesel\Class47\Default\Engine\class47.bin`
* **Parsing Rule:** Whenever resolving a `<BlueprintID>` or `.xml` reference, replace the trailing `.xml` with `.bin` (case-insensitive) before checking the VFS index.

---

## 2. Format Conversion & `serz.exe`

Files with extensions `.bin`, `.GeoPcDx`, `.TgPcDx`, and `.XSec` use Kuju's proprietary binary serialization format (`SERZ` magic header: `53 45 52 5A`).
* Note: Sometimes a `.bin` file in a route/scenario folder is actually plain-text XML with a `.bin` extension. Always check the first 4 bytes: if `b"SERZ"`, it is binary; if `b"<?xm"`, parse directly as XML without calling `serz.exe`.

---

## 3. Exact XML Blueprint Reference Structure

Throughout all route, scenery, loft, and scenario XML files, any reference to an external `.bin` blueprint uses the standard `iBlueprintLibrary-cAbsoluteBlueprintID` block:

```xml
<iBlueprintLibrary-cAbsoluteBlueprintID>
    <BlueprintSetID>
        <iBlueprintLibrary-cBlueprintSetID>
            <Provider d:type="cDeltaString">Kuju</Provider>
            <Product d:type="cDeltaString">RailSimulator</Product>
        </iBlueprintLibrary-cBlueprintSetID>
    </BlueprintSetID>
    <BlueprintID d:type="cDeltaString">Scenery\Buildings\Bath_Temp_WP01_04.xml</BlueprintID>
</iBlueprintLibrary-cAbsoluteBlueprintID>
```

### How to Resolve this Block:
1. Extract `<Provider>` (`Kuju`).
2. Extract `<Product>` (`RailSimulator`).
3. Extract `<BlueprintID>` (`Scenery\Buildings\Bath_Temp_WP01_04.xml`).
4. If `<BlueprintID>` is empty, ignore this block (empty slot).
5. Convert `.xml` to `.bin` -> `Scenery\Buildings\Bath_Temp_WP01_04.bin`.
6. Target VFS path: `Assets\Kuju\RailSimulator\Scenery\Buildings\Bath_Temp_WP01_04.bin`.

---

## 4. Route Directory Structure & Route Checking (`Routes\<Route-GUID>\`)

To perform a complete Route Check (like RW_Tools "Check Route"), TS Checker must scan the following files inside `Content\Routes\<Route-GUID>\`:

### 4.1. `RouteProperties.xml` (Metadata & Core Dependencies)
Always stored as plain XML in the route root.
* **Route Name:** Located under `<DisplayName><Localisation-cUserLocalisedString>` (check `<English>`, `<Russian>`, `<German>`, `<Other>`, etc., taking the first non-empty string).
* **Template Blueprint:** Under `<BlueprintID>` -> points to the Route Template `.bin`.
* **Skies / Weather / Terrain Blueprints:** Contains `<iBlueprintLibrary-cAbsoluteBlueprintID>` references for sky domes, default weather, and terrain texturing (`Environment\Terrain\Texturing.xml`).
* **Required Preload Providers (`<RBlueprintSetPreLoad>`):** Lists all `<Provider>` and `<Product>` pairs enabled in the route editor.

### 4.2. `Scenery\*.bin` (Static 3D Objects on Tiles)
Contains tile files named by coordinate (e.g., `+000001-000002.bin`).
* Each file contains hundreds of placed objects (`cSceneryRibbon`, `cAnimSceneryEntity`, etc.).
* **What to parse:** Extract every `<iBlueprintLibrary-cAbsoluteBlueprintID>` block in every tile file.
* **Result:** This yields all buildings, trees, stations, clutter, and static signs placed on the route.

### 4.3. `Networks\` (Tracks, Roads, and Lofts)
Divided into three subdirectories, each containing a master `*.bin` file and a `* Tiles\` subfolder with coordinate `.bin` files:

1. **`Networks\Track Tiles\*.bin` & `Networks\Tracks.bin`:**
   * `Tracks.bin` holds the global track network ribbon graph, signals, speed limits, track rules (`TrackRule`), and track beds.
   * `Track Tiles\*.bin` hold the visual track geometry placements and junction frogs/motors.
   * **What to parse:** Extract all `<iBlueprintLibrary-cAbsoluteBlueprintID>` blocks (this captures Track Rules, Rail cross-sections, Signals, Level Crossings, Buffers, and Point Motors).
2. **`Networks\Loft Tiles\*.bin` & `Networks\Lofts.bin`:**
   * Defines procedural extruded objects (walls, bridges, fences, embankments, custom catenary wires).
   * **What to parse:** Extract all `<iBlueprintLibrary-cAbsoluteBlueprintID>` blocks inside `Loft Tiles\*.bin`.
3. **`Networks\Road Tiles\*.bin` & `Networks\Roads.bin`:**
   * Defines road splines and car spawners.
   * **What to parse:** Extract all `<iBlueprintLibrary-cAbsoluteBlueprintID>` blocks inside `Road Tiles\*.bin`.

---

## 5. Deep Asset Dependency Chain (`BIN -> GeoPcDx / XSec -> TgPcDx`)

Checking if the `.bin` exists is only Step 1. Community reskins and corrupted asset packs frequently have a valid `.bin` file that points to a missing `.GeoPcDx` (resulting in an invisible object/train) or a `.GeoPcDx` that points to missing `.TgPcDx` textures (resulting in bright white/missing textures).

### 5.1. Step 1: Parsing the `.bin` Blueprint
When a `.bin` file is converted to XML (or binary-scanned), look for visual references:

1. **Standard 3D Geometry (`<GeometryID>`):**
   ```xml
   <GeometryID d:type="cDeltaString">Kuju\RailSimulator\RailVehicles\Diesel\Class47\Default\Engine\[00]class47</GeometryID>
   ```
   * **The `[00]` Prefix Quirk:** TSC inserts `[00]` (or `[01]`, etc. for LODs) right before the filename in `<GeometryID>` and `<CollisionGeometryID>`.
   * **Resolution Rule:**
     1. Strip `[00]` from the filename -> `Kuju\RailSimulator\RailVehicles\Diesel\Class47\Default\Engine\class47`.
     2. Append `.GeoPcDx` -> `Assets\Kuju\RailSimulator\RailVehicles\Diesel\Class47\Default\Engine\class47.GeoPcDx`.

2. **Loft Cross-Sections (`<CrossSectionID>`):**
   Found inside `cLoftBlueprint`, `cTrackSectionBlueprint`, and `cRoadSectionBlueprint`:
   ```xml
   <CrossSectionID d:type="cDeltaString">Kuju\RailSimulator\RailNetwork\Track\bath_temp_track01</CrossSectionID>
   ```
   * **Resolution Rule:** Append `.XSec` -> `Assets\Kuju\RailSimulator\RailNetwork\Track\bath_temp_track01.XSec`.
   * *Note:* Loft blueprints also contain `<StartGeometryBPID>`, `<MiddleGeometryBPID>`, and `<EndGeometryBPID>` (which are nested `<iBlueprintLibrary-cAbsoluteBlueprintID>` blocks pointing to child `.bin` blueprints for bridge pillars, fence posts, etc.).

3. **Child Blueprints (`<cEntityContainerBlueprint-sChild>`):**
   Rolling stock and complex scenery attach child blueprints (e.g., driver models, lights,freight loads, auto-numbers, pantographs).
   * **Resolution Rule:** Any `<iBlueprintLibrary-cAbsoluteBlueprintID>` inside `<Children>` must be recursively checked just like a root `.bin`.

### 5.2. Step 2: Parsing `.GeoPcDx` (3D Mesh -> Textures)
Inside a `.GeoPcDx` file (when converted via `serz.exe` or parsed directly), materials point to texture files inside `<TextureByName>`:

```xml
<TextureByName>
    <e d:type="cDeltaString">textures\[00]class47_body</e>
</TextureByName>
```
* **Resolution Rule for `.GeoPcDx` Textures:**
  1. The path inside `<e>` is **relative to the directory containing the `.GeoPcDx` file**!
  2. Strip the `[00]` prefix from the filename -> `textures\class47_body`.
  3. Resolve relative navigation (`..\` or `./`) against the parent folder of the `.GeoPcDx`.
  4. Append `.TgPcDx`.
  5. *Example:* If `.GeoPcDx` is in `Assets\Provider\Product\Scenery\house.GeoPcDx` and `<e>` is `textures\[00]wall`, the texture path is `Assets\Provider\Product\Scenery\textures\wall.TgPcDx`.

### 5.3. Step 3: Parsing `.XSec` (Loft Cross-Section -> Textures)
Loft `.XSec` files do not use `.GeoPcDx` for their extruded mesh; instead, the `.XSec` file itself references the textures directly.
* Inside an `.XSec` file (converted via `serz.exe`), look for `<TextureName>` or `<TextureByName>` tags containing strings like `Kuju\RailSimulator\RailNetwork\Track\textures\[00]ballast` or relative paths `textures\[00]ballast`.
* **Resolution Rule:** Strip `[00]`, resolve relative or `Provider\Product` path, and append `.TgPcDx`.

---

## 6. Scenarios & Consist Inspection (`Content\Routes\<Route-GUID>\Scenarios\<Scenario-GUID>\`)

TS Checker must list scenarios, inspect consists (trains), verify rolling stock integrity, and swap missing locomotives/wagons.

### 6.1. Scenario Files Overview
Each folder inside `Scenarios\` is a GUID representing one scenario:
* **`ScenarioProperties.xml`:** Plain XML file containing:
  * `<DisplayName>` (Scenario title).
  * `<Description>` and `<Briefing>`.
  * `<StartTime>`, `<DurationMins>`, `<Season>`, `<WeatherBlueprint>`.
  * `<CareerRules>` / `<ScenarioClass>` (Standard, Career, Free Roam, Quick Drive, Timetabled).
* **`Scenario.bin` (or `Scenario.xml`):** Contains all placed consists (trains), AI driver instructions, and scenario-specific scenery objects.
* **`CurrentSave.bin` / `InitialSave.bin`:** Saved states (can be ignored during standard static verification unless checking saved games).

### 6.2. How Consists & Rolling Stock are Stored in `Scenario.bin`
Inside `Scenario.bin`, every train on the map is defined as a `<cConsist>` block inside `<Record>`:

1. **Identifying a Train / Service (`<cConsist>`):**
   * **Driver / Service Name:** Located inside `<Driver><cDriver><ServiceName><Localisation-cUserLocalisedString>` (e.g., `1S26 King's Cross - Edinburgh`). If `<Driver>` is empty/null, it is a **Loose Consist** (static parked wagons in a yard).
   * **Player Train Flag:** `<PlayerDriver d:type="bool">1</PlayerDriver>` inside `<cDriver>` indicates this is the train driven by the player!

2. **Vehicles Inside the Consist (`<RailVehicles>`):**
   Inside `<cConsist>`, the `<RailVehicles>` node contains an ordered list of `<cOwnedEntity>` elements (each representing one locomotive, tender, or wagon):

   ```xml
   <cOwnedEntity d:id="123456">
       <Component>
           <!-- Various physics, cargo, and position components -->
           <cEngineSimComponent/> <!-- Present if locomotive -->
           <cWagonSimComponent/>  <!-- Present if unpowered wagon -->
           <cSimulationEntity/>
       </Component>
       <BlueprintID>
           <iBlueprintLibrary-cAbsoluteBlueprintID>
               <BlueprintSetID>
                   <iBlueprintLibrary-cBlueprintSetID>
                       <Provider d:type="cDeltaString">AP</Provider>
                       <Product d:type="cDeltaString">Class43Pack01</Product>
                   </iBlueprintLibrary-cBlueprintSetID>
               </BlueprintSetID>
               <BlueprintID d:type="cDeltaString">RailVehicles\Diesel\HST\GWR\Class43\class43_gwr.xml</BlueprintID>
           </iBlueprintLibrary-cAbsoluteBlueprintID>
       </BlueprintID>
       <Name d:type="cDeltaString">cRailVehicle</Name>
   </cOwnedEntity>
   ```

3. **Vehicle Numbering & Flipped State:**
   * `<UniqueNumber d:type="cDeltaString">43185</UniqueNumber>` (inside `<cCargoComponent>` or vehicle properties) defines the running number on the side of the unit.
   * `<Flipped d:type="bool">1</Flipped>` (inside `<Followers>` / `<cConsist>` entry) indicates if the vehicle is reversed on the track.

### 6.3. Rolling Stock Integrity Check (What makes a train "broken"?)
For every `<cOwnedEntity>` in a scenario's `<cConsist>`:
1. **Missing `.bin`:** The file `Assets\<Provider>\<Product>\<BlueprintID>.bin` does not exist. (Causes "Consist Builder - Can't load rail vehicle" error on scenario start).
2. **Missing `.GeoPcDx`:** The `.bin` exists (very common with community reskins where the user forgot to run `InstallMe.bat` to copy the `.GeoPcDx` from the original payware pack), so `<GeometryID>` points to a non-existent `.GeoPcDx`. (Result: Invisible train in-game, only couplers show).
3. **Missing Bogie Blueprints (`<Bogie>`):** Inside a RailVehicle `.bin`, `<Bogies>` reference separate `.bin` files for wheel trucks. If missing, the train crashes or floats.
4. **Missing Child Blueprints / Textures:** Missing `.TgPcDx` textures inside the vehicle's `.GeoPcDx`.

---

## 7. Asset & Rolling Stock Replacement Mechanics

One of the core features of TS Checker (replacing RW_Tools) is allowing the user to swap a missing scenery object, track, or train vehicle with an existing one.

### 7.1. Replacing Scenery, Lofts, or Tracks in a Route
To replace Asset A (`ProviderA`, `ProductA`, `PathA.xml`) with Asset B (`ProviderB`, `ProductB`, `PathB.xml`) across a route:
1. Scan target `.bin` files (`Scenery\*.bin`, `Loft Tiles\*.bin`, or `Track Tiles\*.bin`).
2. Convert the tile `.bin` to XML (via `serz.exe` or direct binary stream manipulation).
3. Locate every `<iBlueprintLibrary-cAbsoluteBlueprintID>` where:
   * `<Provider>` == `ProviderA`
   * `<Product>` == `ProductA`
   * `<BlueprintID>` == `PathA.xml` (ensure `.xml` extension is preserved in the tag!).
4. Replace those three values with `ProviderB`, `ProductB`, and `PathB.xml`.
5. **PreLoad Update:** Ensure `ProviderB` / `ProductB` is added to `<RBlueprintSetPreLoad>` in `RouteProperties.xml` (if not already present), otherwise the game may fail to load the replacement asset into memory!
6. Recompile the XML back to `.bin` via `serz.exe`.

### 7.2. Replacing Rolling Stock in a Scenario (`Scenario.bin`)
Replacing a locomotive or wagon in `Scenario.bin` requires updating two places to prevent scenario corruption:
1. **Update `<cOwnedEntity>`:** Inside the `<cConsist>`, replace `<Provider>`, `<Product>`, and `<BlueprintID>` (keeping the `.xml` suffix) with the new rolling stock's values.
2. **Update Vehicle Running Number (`<UniqueNumber>`):**
   * Each vehicle blueprint has an `<CsvFile>` (`dcsv` file) defining valid running numbers.
   * When swapping Vehicle A with Vehicle B, if Vehicle A's running number format is incompatible with Vehicle B's auto-numbering script, textures on the number boards may glitch. Best practice: read one valid number from Vehicle B's `.dcsv` file (referenced in `<NumberingList>` inside Vehicle B's `.bin`) and insert it into `<UniqueNumber>` in `Scenario.bin`.
3. **Update Scenario/Route PreLoad:** Add the new `<Provider>` and `<Product>` to `<RBlueprintSetPreLoad>` in `ScenarioProperties.xml` so the scenario engine mounts the new rolling stock's `.ap` / asset folder!

---

## 8. Encoding & Umlaut / Special Character Corruption (`ä`, `ö`, `ü`, `ß`)

European (especially German, French, Czech, and Russian) routes and asset packs frequently break due to character encoding mismatches between ZIP archivers (CP850 / CP437 / Windows-1252) and UTF-8 XML strings.

### 8.1. Where Corruption Happens:
1. **Disk vs XML Mismatch:** The `.bin` file (UTF-8 XML) requests `Scenery\Bäume\Buche_01.xml`, but on disk, unpacking an old `.rwp` or `.zip` turned the folder name into `Scenery\B├¤ume\Buche_01.bin` or `Scenery\B?ume\Buche_01.bin`.
2. **`serz.exe` Failure on Non-ASCII:** Older versions of `serz.exe` or specific Windows regional locales fail when passed command-line paths containing non-ASCII characters or corrupted byte sequences.

### 8.2. TS Checker Detection & Fix Algorithm:
1. **Detection:**
   * Flag any asset path on disk or inside an XML reference containing non-ASCII characters (`!path.is_ascii()`), specifically German umlauts (`ä, ö, ü, Ä, Ö, Ü, ß`), accented characters, or mojibake bytes (`├`, ``, `?`).
   * Distinguish between:
     * *Warning (Valid Non-ASCII):* The file exists and matches the XML reference, but uses non-ASCII characters that may break on other locales.
     * *Error (Broken Encoding Match):* The XML requests an umlaut path that is missing on disk, while a similarly named file with garbled characters exists in the same directory.
2. **Automated Repair:**
   * **Case A (Fix Disk Filename to Match Blueprint):** If the `.bin` / `.GeoPcDx` expects `Bäume` and the disk has `B├¤ume`, rename the file/folder on disk to match the valid string in the blueprint.
   * **Case B (Full Sanitization to ASCII):** If normalizing to ASCII (`ä -> ae`, `ö -> oe`, `ü -> ue`, `ß -> ss`), TS Checker must update the entire chain simultaneously:
     1. Rename `.TgPcDx`, `.GeoPcDx`, `.XSec`, and `.bin` files on disk.
     2. Rewrite internal `<TextureByName>` strings inside `.GeoPcDx` / `.XSec`.
     3. Rewrite `<GeometryID>` / `<CrossSectionID>` inside `.bin`.
     4. Rewrite `<BlueprintID>` inside the route's `Scenery\*.bin`, `Tracks.bin`, and `Scenario.bin`.