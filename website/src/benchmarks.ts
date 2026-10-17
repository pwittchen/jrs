// The benchmarks page: the latest pass of `cargo bench --bench compare`
// (specs/BENCHMARKS_SUMMARY.md), drawn from one table of numbers so the bars
// and the tables cannot disagree. Keep RESULTS in step with that file.

import { $, $$, setupCopy, stampVersion } from "./shared";

type Tool = "jrs" | "maven" | "gradle" | "gradleCc" | "gradleNoDaemon" | "gradleParallel";

/** `short` names a bar, where the value beside it leaves less room. */
const TOOLS: { id: Tool; name: string; short: string }[] = [
  { id: "jrs", name: "jrs", short: "jrs" },
  { id: "maven", name: "Maven", short: "Maven" },
  { id: "gradle", name: "Gradle (daemon)", short: "Gradle daemon" },
  { id: "gradleCc", name: "Gradle (daemon, CC)", short: "Gradle CC" },
  { id: "gradleNoDaemon", name: "Gradle (no daemon)", short: "Gradle no daemon" },
  { id: "gradleParallel", name: "Gradle (daemon, --parallel)", short: "Gradle --parallel" },
];

/**
 * Milliseconds per tool; `null` when the scenario was not timed. Gradle
 * `--parallel` is timed on workspaces only, so other rows leave it out.
 */
type Row = [
  scenario: string,
  jrs: number | null,
  maven: number,
  gradle: number,
  gradleCc: number,
  gradleNoDaemon: number,
  gradleParallel?: number,
];

interface Project {
  id: string;
  name: string;
  what: string;
  rows: Row[];
}

const RESULTS: Project[] = [
  {
    id: "wordstats",
    name: "wordstats",
    what: "a few classes, Guava, JUnit",
    rows: [
      ["cold", 1300, 1560, 11200, 11500, 11000],
      ["clean", 287, 1190, 433, 417, 2330],
      ["noop", 61, 1050, 358, 312, 2120],
      ["incremental", 245, 1170, 392, 369, 2300],
      ["abi", 304, 1260, 388, 369, 2320],
      ["test", 354, 1240, 595, 577, 2380],
      ["test-edit", 524, 1530, 645, 689, 2610],
      ["run", 127, 1040, 429, 443, 2100],
      ["package", 219, 1550, 606, 646, 2370],
    ],
  },
  {
    id: "bookmarks",
    name: "bookmarks",
    what: "Spring Boot 4.1, MockMvc tests",
    rows: [
      ["cold", 1340, 1940, 12400, 12600, 12100],
      ["clean", 282, 1270, 565, 601, 2580],
      ["noop", 62, 1020, 445, 325, 2240],
      ["incremental", 256, 1260, 490, 388, 2550],
      ["abi", 338, 1270, 447, 373, 2530],
      ["test", 1050, 2290, 1600, 1530, 3590],
      ["test-edit", 1250, 2640, 1660, 1720, 3890],
      ["run", 740, 2080, 1000, 1060, 2920],
      ["package", 184, 1550, 521, 536, 2430],
    ],
  },
  {
    id: "gen-300",
    name: "gen-300",
    what: "300 classes, 30 dependencies",
    rows: [
      ["cold", 1340, 1980, 12200, 12600, 12000],
      ["clean", 584, 1530, 605, 598, 2700],
      ["noop", 64, 964, 356, 311, 2050],
      ["incremental", 253, 1540, 413, 386, 2340],
      ["abi", 424, 1540, 450, 399, 2430],
      ["test", 320, 1320, 640, 609, 2420],
      ["test-edit", 505, 2000, 709, 756, 2810],
      ["run", 127, 1080, 406, 432, 2100],
      ["package", 182, 2320, 1480, 1520, 3290],
    ],
  },
  {
    id: "gen-2000",
    name: "gen-2000",
    what: "2000 classes, 60 dependencies",
    rows: [
      ["cold", 1760, 3230, 14400, 14300, 13800],
      ["clean", 1480, 2440, 1250, 1210, 3810],
      ["noop", 82, 1090, 354, 315, 2160],
      ["incremental", 382, 2630, 583, 519, 2820],
      ["abi", 817, 2570, 622, 613, 2940],
      ["test", 720, 1750, 804, 802, 2790],
      ["test-edit", 1030, 3350, 1050, 1080, 3430],
      ["run", 234, 1180, 501, 520, 2290],
      ["package", 340, 3450, 2530, 2610, 4440],
    ],
  },
  {
    id: "gen-ws",
    name: "gen-ws",
    what: "a six-member workspace, 300 classes",
    rows: [
      ["cold", 1510, 2430, 14500, 14400, 14300, 14200],
      ["clean", 944, 1770, 789, 777, 3140, 778],
      ["noop", 97, 1010, 397, 329, 2250, 406],
      ["incremental", 223, 1320, 441, 385, 2540, 434],
      ["abi", 534, 1710, 499, 461, 2660, 478],
      ["test", 456, 2390, 1860, 711, 3800, 737],
      ["test-edit", 436, 2700, 676, 747, 2870, 645],
      ["package", 318, 3150, 1480, 1570, 3430, 1510],
    ],
  },
];

/** The releases every pass in specs/BENCHMARKS_SUMMARY.md was run with, oldest first. */
const RELEASES = ["0.11.0", "0.12.0", "0.13.0", "0.14.0", "0.14.1", "0.14.2", "0.14.3"];

/**
 * jrs's milliseconds and the warm Gradle daemon's, one per release in
 * RELEASES order. The daemon did not change between passes, so its line is
 * how far the machine moved; jrs's last column is RESULTS's.
 */
type Trend = [scenario: string, jrs: number[], gradle: number[]];

const HISTORY: Record<string, Trend[]> = {
  wordstats: [
    ["cold", [557, 554, 973, 424, 980, 1300, 1340], [11300, 11200, 11200, 11700, 11500, 11200, 11200]],
    ["clean", [325, 322, 250, 1010, 340, 287, 271], [454, 444, 450, 455, 480, 433, 451]],
    ["noop", [63, 63, 62, 63, 64, 61, 62], [357, 342, 350, 347, 373, 358, 340]],
    ["incremental", [245, 245, 849, 215, 229, 245, 224], [392, 392, 387, 389, 416, 392, 388]],
    ["abi", [426, 417, 367, 364, 381, 304, 300], [382, 389, 391, 389, 401, 388, 386]],
    ["test", [369, 358, 354, 360, 361, 354, 357], [604, 609, 614, 611, 598, 595, 607]],
    ["test-edit", [543, 546, 501, 515, 556, 524, 520], [652, 635, 643, 650, 662, 645, 633]],
    ["run", [126, 126, 123, 132, 126, 127, 125], [439, 415, 421, 433, 426, 429, 397]],
    ["package", [207, 206, 88, 89, 108, 219, 88], [600, 604, 607, 589, 614, 606, 598]],
  ],
  bookmarks: [
    ["cold", [632, 643, 1060, 513, 1030, 1340, 1250], [12300, 12300, 12100, 13000, 13400, 12400, 12500]],
    ["clean", [336, 342, 329, 1030, 346, 282, 291], [558, 574, 578, 569, 656, 565, 556]],
    ["noop", [63, 62, 62, 63, 68, 62, 66], [450, 453, 426, 448, 453, 445, 476]],
    ["incremental", [285, 296, 880, 252, 261, 256, 270], [489, 483, 492, 498, 493, 490, 505]],
    ["abi", [475, 471, 404, 406, 431, 338, 343], [453, 457, 453, 461, 464, 447, 461]],
    ["test", [1080, 1080, 1060, 1090, 1080, 1050, 1090], [1630, 1650, 1600, 1630, 1630, 1600, 1700]],
    ["test-edit", [1300, 1270, 1280, 1290, 1300, 1250, 1280], [1650, 1610, 1640, 1650, 1670, 1660, 1630]],
    ["run", [762, 738, 758, 756, 802, 740, 759], [1010, 1010, 1010, 1000, 1010, 1000, 1000]],
    ["package", [719, 753, 201, 173, 211, 184, 188], [513, 515, 494, 512, 514, 521, 509]],
  ],
  "gen-300": [
    ["cold", [891, 906, 1360, 767, 1050, 1340, 1300], [12300, 12100, 12000, 12500, 12700, 12200, 12200]],
    ["clean", [673, 684, 604, 1350, 588, 584, 583], [640, 613, 651, 668, 616, 605, 669]],
    ["noop", [66, 65, 65, 66, 71, 64, 69], [370, 356, 357, 373, 372, 356, 373]],
    ["incremental", [271, 264, 844, 242, 253, 253, 250], [432, 421, 433, 435, 440, 413, 424]],
    ["abi", [843, 852, 501, 493, 517, 424, 424], [461, 466, 432, 434, 434, 450, 469]],
    ["test", [474, 450, 446, 452, 315, 320, 345], [654, 655, 642, 715, 722, 640, 635]],
    ["test-edit", [671, 655, 593, 687, 486, 505, 523], [709, 704, 688, 706, 698, 709, 697]],
    ["run", [132, 131, 126, 126, 132, 127, 132], [438, 398, 396, 416, 415, 406, 425]],
    ["package", [758, 731, 242, 190, 207, 182, 193], [1500, 1480, 1460, 1480, 1560, 1480, 1540]],
  ],
  "gen-2000": [
    ["cold", [1850, 1910, 2280, 1670, 1760, 1760, 1770], [14200, 14300, 14000, 14800, 14700, 14400, 14600]],
    ["clean", [1550, 1530, 1450, 2180, 1540, 1480, 1560], [1260, 1280, 1240, 1230, 1270, 1250, 1240]],
    ["noop", [85, 83, 85, 89, 84, 82, 82], [388, 377, 364, 362, 363, 354, 357]],
    ["incremental", [386, 394, 1110, 349, 380, 382, 332], [593, 583, 575, 600, 578, 583, 583]],
    ["abi", [1840, 1870, 822, 810, 895, 817, 738], [641, 654, 623, 634, 638, 622, 664]],
    ["test", [910, 901, 856, 893, 763, 720, 849], [851, 831, 837, 842, 879, 804, 843]],
    ["test-edit", [1200, 1190, 1170, 1190, 1030, 1030, 1030], [1060, 1030, 1060, 1070, 1070, 1050, 1040]],
    ["run", [237, 218, 237, 232, 226, 234, 232], [507, 496, 495, 497, 494, 501, 495]],
    ["package", [1410, 1400, 486, 362, 356, 340, 376], [2580, 2560, 2550, 2540, 2530, 2530, 2610]],
  ],
  "gen-ws": [
    ["cold", [2220, 1900, 1820, 1380, 1490, 1490, 1510], [13900, 13700, 13800, 14100, 14200, 13900, 14500]],
    ["clean", [1980, 1700, 1130, 2350, 817, 826, 944], [791, 803, 800, 768, 746, 780, 789]],
    ["noop", [81, 83, 81, 86, 82, 83, 97], [395, 400, 391, 388, 394, 393, 397]],
    ["incremental", [287, 278, 303, 207, 210, 205, 223], [443, 439, 434, 435, 433, 414, 441]],
    ["abi", [2090, 1770, 667, 511, 512, 516, 534], [519, 505, 492, 506, 504, 488, 499]],
    ["test", [1890, 1800, 1810, 574, 419, 438, 456], [1830, 1850, 1920, 1810, 1790, 1780, 1860]],
    ["test-edit", [626, 620, 642, 540, 424, 411, 436], [681, 670, 692, 703, 658, 669, 676]],
    ["package", [803, 818, 309, 258, 246, 239, 318], [1490, 1490, 1540, 1490, 1480, 1500, 1480]],
  ],
};

// The Maven and Gradle marks, from Simple Icons (simpleicons.org, CC0), on a
// 24-unit square. jrs's own is the logo the top bar already loads.
const MARKS: Partial<Record<Tool, string>> = {
  maven: "M4.237.001c-.312-.013-.665.072-.828.457-.158.374-.283 1.188-.34 2.276l1.223.591c-.02-.737.007-1.43.076-2.066-.026.299-.056.96.006 2.039.019.342.049.725.088 1.15.002.024.002.047.007.069a45.485 45.485 0 0 0 .309 2.412c.057.368.126.752.195 1.16l-.01.01c.014.01.015.018.014.023l.03.16c.03.162.06.328.093.494l.108.553.056.289a61.72 61.72 0 0 0 .457 2.068c.09.382.186.78.287 1.186.098.386.199.783.309 1.193.096.362.199.735.303 1.117.003.018.012.036.015.055a145.826 145.826 0 0 0 .34 1.185l.049.174c.078.261.158.533.242.805a4.2 4.2 0 0 1-.293-.135l-.19-.654c-.02-.077-.042-.148-.062-.225l-.002-.004-.004-.002c-.087-.3-.17-.607-.257-.916-.023-.087-.044-.173-.069-.263l-.314-1.178c-.1-.381-.194-.765-.29-1.154-.094-.39-.185-.78-.277-1.172-.093-.401-.181-.8-.265-1.203-.085-.396-.161-.798-.24-1.193a50.315 50.315 0 0 1-.211-1.17c-.004-.013-.006-.03-.01-.041l.004-.002c-.057-.386-.116-.77-.174-1.15a60.905 60.905 0 0 1-.154-1.204 27.447 27.447 0 0 1-.172-2.41l-1.22-.59c-.004.074-.01.15-.013.23-.012.294-.02.605-.023.93a45.3 45.3 0 0 0 .006 1.157c.009.37.025.755.045 1.148.02.336.042.675.07 1.022l.002.039.006.004c.003.023.007.05.006.076.033.368.064.739.107 1.115a34.493 34.493 0 0 0 .303 2.125c.01.064.024.131.035.195a23.418 23.418 0 0 0 .547 2.32c.07.237.14.464.21.68.063.182.13.365.194.545.155.422.327.832.512 1.232l.006.004a.318.318 0 0 0 .02.05c.225.485.475.95.755 1.395.01.013.02.033.03.047-.455-.183-1.259-.098-1.253-.097.83.288 1.557.64 2.016 1.175-.183.2-.523.352-.953.477.594.064.924-.039 1.045-.092-.31.26-.483.732-.635 1.24.35-.57.696-.949 1.033-1.094.078.258.162.524.244.788A147.532 147.532 0 0 0 5.157 24a.56.56 0 0 0 .43-.312c.13-.282.83-1.775 1.908-3.875.413 1.303.88 2.679 1.386 4.109a.494.494 0 0 0 .076-.465 103.735 103.735 0 0 1-1.308-3.945c.154-.299.316-.612.484-.932.125.04.255.094.389.155.203.186.352.491.482.84a1.515 1.515 0 0 0-.334-1.098c1.335.258 2.547.09 3.287-.81a3.97 3.97 0 0 0 .192-.258c-.325.304-.682.404-1.313.273.996-.281 1.523-.617 2.035-1.22.12-.145.244-.303.371-.48-.943.722-1.927.822-2.9.493l-.045-.018c.914.02 2.203-.474 3.092-1.189.41-.33.796-.73 1.17-1.21.28-.359.55-.76.82-1.216.234-.393.468-.824.7-1.293a2.83 2.83 0 0 1-.74.137l-.144.008c-.048.002-.093 0-.146.002.885-.198 1.5-.74 1.994-1.447-.24.117-.628.262-1.07.297-.058.006-.12.006-.182.006-.013-.002-.028 0-.047-.002.306-.078.574-.178.81-.309a3.363 3.363 0 0 0 .358-.236c.044-.037.088-.07.13-.106.099-.086.193-.18.28-.287.028-.034.056-.063.08-.098.036-.05.073-.098.104-.146a8.388 8.388 0 0 0 .51-.828c.015-.031.032-.057.046-.088.04-.084.08-.16.11-.227.042-.099.074-.179.092-.238a.515.515 0 0 1-.108.051c-.273.112-.727.187-1.086.201-.004 0-.008 0-.013.004h-.067c.72-.214 1.067-.45 1.422-.818a13.883 13.883 0 0 0 1.154-1.428c.264-.37.505-.738.692-1.072a6.5 6.5 0 0 0 .298-.592c.066-.157.122-.305.172-.45-.466.01-.986.011-1.48 0 .495.01 1.015.007 1.484-.005.5-1.485.063-2.262.063-2.262s-.526-1.212-1.4-.851c-.426.175-1.172.73-2.083 1.56l.514 1.45a17.561 17.561 0 0 1 1.703-1.602c-.257.22-.807.726-1.615 1.644-.256.29-.537.624-.844.997-.017.02-.035.038-.047.06a51.435 51.435 0 0 0-1.666 2.187c-.248.34-.498.704-.765 1.088h-.016c.002.02-.004.028-.01.032l-.101.152c-.104.155-.213.31-.318.47l-.352.534c-.061.09-.124.181-.186.277-.184.282-.367.573-.558.873a97.351 97.351 0 0 0-1.428 2.338 96.866 96.866 0 0 0-1.341 2.343c-.012.017-.02.04-.034.057a197.256 197.256 0 0 0-.668 1.223l-.097.181c-.17.318-.346.642-.52.979 0 .004-.005.008-.006.013-.026.048-.05.093-.072.141-.117.222-.218.424-.45.87a1.352 1.352 0 0 0-.233-.182l.345-.65c.047-.089.096-.177.143-.27l.04-.077.546-1.001.13-.233v-.006l-.001-.006c.169-.31.345-.62.52-.94.051-.087.102-.173.153-.265.224-.395.454-.794.684-1.197a91.685 91.685 0 0 1 2.135-3.504c.247-.386.503-.77.754-1.152.092-.138.182-.272.279-.41a72.9 72.9 0 0 1 .48-.701c.007-.012.019-.024.026-.037h.006c.26-.356.517-.713.773-1.065.278-.373.554-.735.83-1.09a31.075 31.075 0 0 1 1.777-2.075l-.515-1.446c-.06.057-.126.116-.192.178a32.37 32.37 0 0 0-.758.729c-.295.294-.597.606-.912.935a46.032 46.032 0 0 0-1.632 1.838l-.03.033.002.008c-.017.02-.033.044-.054.064-.266.323-.538.649-.801.985a39.105 39.105 0 0 0-1.445 1.95c-.043.06-.085.126-.127.186a26.458 26.458 0 0 0-1.403 2.303c-.13.247-.256.485-.37.715-.096.195-.187.395-.278.591-.21.463-.398.93-.566 1.399l.002.006a.36.36 0 0 0-.026.058c-.108.303-.203.608-.29.914-.14.174-.302.325-.483.46a3.505 3.505 0 0 0-.131-.153 5.148 5.148 0 0 0 .824-2.211 6.4 6.4 0 0 0-.016-1.488c-.046-.4-.126-.82-.238-1.274-.097-.393-.217-.81-.363-1.248-.091.185-.22.367-.379.545l-.086.094c-.029.032-.06.06-.092.094.434-.674.486-1.397.358-2.148a2.722 2.722 0 0 1-.49.85c-.033.038-.072.077-.11.116-.01.007-.019.018-.033.028.144-.24.25-.467.318-.698a1.29 1.29 0 0 0 .04-.146 2.85 2.85 0 0 0 .038-.225l.018-.146a2.11 2.11 0 0 0-.002-.354c-.003-.04-.004-.076-.01-.113-.01-.055-.016-.105-.027-.154a7.416 7.416 0 0 0-.193-.84c-.01-.028-.015-.056-.026-.084-.027-.079-.048-.149-.072-.209a2.1 2.1 0 0 0-.09-.209.455.455 0 0 1-.035.1c-.102.24-.34.57-.557.8-.003.003-.007.005-.007.01l-.04.043c.318-.58.39-.946.385-1.398a12.274 12.274 0 0 0-.16-1.615 10.68 10.68 0 0 0-.232-1.104 5.853 5.853 0 0 0-.18-.558 6.337 6.337 0 0 0-.172-.391 26.18 26.18 0 0 0 .002-.004C5.576.341 4.82.124 4.82.124s-.27-.11-.582-.123zm3.38 15.783l.032.082v.002c-.06.033-.116.067-.178.097-.012.004-.024.012-.039.018a2.41 2.41 0 0 0 .186-.2zm-.603 1.626c.13.136.25.242.354.32l.07.227a1.866 1.866 0 0 0-.246.053l-.03-.098c-.024-.084-.048-.17-.076-.257l-.021-.073zm.26.875a2.34 2.34 0 0 1 .271.01l.07.229a.778.778 0 0 1 .247-.004l-.326.627a127.643 127.643 0 0 1-.262-.862z",
  gradle: "M22.695 4.297a3.807 3.807 0 0 0-5.29-.09.368.368 0 0 0 0 .533l.46.47a.363.363 0 0 0 .474.032 2.182 2.182 0 0 1 2.86 3.291c-3.023 3.02-7.056-5.447-16.211-1.083a1.24 1.24 0 0 0-.534 1.745l1.571 2.713a1.238 1.238 0 0 0 1.681.461l.037-.02-.029.02.688-.384a16.083 16.083 0 0 0 2.193-1.635.384.384 0 0 1 .499-.016.357.357 0 0 1 .016.534 16.435 16.435 0 0 1-2.316 1.741H8.77l-.696.39a1.958 1.958 0 0 1-.963.25 1.987 1.987 0 0 1-1.726-.989L3.9 9.696C1.06 11.72-.686 15.603.26 20.522a.363.363 0 0 0 .354.296h1.675a.363.363 0 0 0 .37-.331 2.478 2.478 0 0 1 4.915 0 .36.36 0 0 0 .357.317h1.638a.363.363 0 0 0 .357-.317 2.478 2.478 0 0 1 4.914 0 .363.363 0 0 0 .358.317h1.627a.363.363 0 0 0 .363-.357c.037-2.294.656-4.93 2.42-6.25 6.108-4.57 4.502-8.486 3.088-9.9zm-6.229 6.901l-1.165-.584a.73.73 0 1 1 1.165.587z",
};
MARKS.gradleCc = MARKS.gradle;
MARKS.gradleNoDaemon = MARKS.gradle;
MARKS.gradleParallel = MARKS.gradle;

const SVG = "http://www.w3.org/2000/svg";

/** A table header: the tool's mark, then its name. */
function toolHead(tool: { id: Tool; name: string }): HTMLElement {
  const th = el("th", tool.id === "jrs" ? "jrs" : undefined);
  const head = el("span", "tool-head");
  if (tool.id === "jrs") {
    const logo = $<HTMLImageElement>(".brand img");
    if (logo) {
      const img = el("img", "tool-mark");
      img.src = logo.src;
      img.alt = "";
      head.append(img);
    }
  } else if (MARKS[tool.id]) {
    const svg = document.createElementNS(SVG, "svg");
    svg.setAttribute("viewBox", "0 0 24 24");
    svg.setAttribute("aria-hidden", "true");
    svg.classList.add("tool-mark");
    const path = document.createElementNS(SVG, "path");
    path.setAttribute("d", MARKS[tool.id]!);
    svg.append(path);
    head.append(svg);
  }
  head.append(document.createTextNode(tool.name));
  th.append(head);
  return th;
}

const SCENARIOS: Record<string, string> = {
  cold: "build, empty caches",
  clean: "build, warm caches, clean tree",
  noop: "build again, nothing changed",
  incremental: "build after a private edit",
  abi: "build after a public API change",
  test: "the whole suite",
  "test-edit": "test after an edit",
  run: "to the first output",
  package: "the fat jar",
};

function format(ms: number): string {
  if (ms < 1000) return `${ms} ms`;
  if (ms < 10000) return `${(ms / 1000).toFixed(2)} s`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function ratio(theirs: number, jrs: number): string {
  const r = theirs / jrs;
  return r >= 10 ? `${r.toFixed(0)}×` : `${r.toFixed(1)}×`;
}

/** How much faster another tool was than jrs, where it was. */
function faster(theirs: number, jrs: number): string {
  const r = jrs / theirs;
  return r < 1.1 ? `${Math.round((1 - theirs / jrs) * 100)}% faster` : `${r.toFixed(1)}× faster`;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, text?: string): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text !== undefined) node.textContent = text;
  return node;
}

/** A horizontal bar per tool for one project's scenario, on a linear scale from zero. */
function renderBars(host: HTMLElement): void {
  const project = RESULTS.find((p) => p.id === host.dataset.project);
  const row = project?.rows.find((r) => r[0] === host.dataset.scenario);
  if (!project || !row) return;
  const values = row.slice(1) as (number | null)[];
  const max = Math.max(...values.map((v) => v ?? 0));
  const jrs = values[0];
  // A chart marked data-best draws its fastest tool in yellow, as the tables do.
  const best = host.dataset.best !== undefined ? Math.min(...values.filter((v): v is number => v != null)) : null;
  const list = el("ol", "bars");
  TOOLS.forEach((tool, i) => {
    const value = values[i];
    if (value === null || value === undefined) return;
    const item = el("li", tool.id === "jrs" ? "bar jrs" : "bar");
    if (value === best) item.classList.add("best");
    const compared = jrs && tool.id !== "jrs" ? (value < jrs ? faster(value, jrs) : ratio(value, jrs)) : "";
    const versus = !compared ? "" : value < jrs! ? `, ${compared} than jrs` : `, ${compared} jrs's time`;
    item.tabIndex = 0;
    item.setAttribute("aria-label", `${tool.name}: ${format(value)}${versus}${value === best ? " (fastest)" : ""}`);
    item.append(el("span", "bar-name", tool.short));
    const track = el("span", "bar-track");
    const fill = el("span", "bar-fill");
    fill.style.width = `${Math.max((value / max) * 100, 0.6)}%`;
    track.append(fill);
    const shown = el("span", "bar-value", format(value));
    if (compared) shown.append(el("span", value < jrs! ? "x ahead" : "x", compared));
    item.append(track, shown);
    item.append(el("span", "bar-tip", `${tool.name} · ${format(value)}${versus}`));
    list.append(item);
  });
  host.append(list);
}

/** One table per project: every scenario, every tool, the ratio to jrs. */
function renderTables(host: HTMLElement): void {
  for (const project of RESULTS) {
    const figure = el("figure", "results");
    const caption = el("figcaption");
    caption.append(el("strong", undefined, project.name), document.createTextNode(` · ${project.what}`));
    figure.append(caption);
    const scroll = el("div", "table-scroll");
    const table = el("table");
    const head = el("tr");
    head.append(el("th", "scenario", "Scenario"));
    // The tools this project was timed with: a column only some rows have
    // is still a column for all of them.
    const tools = TOOLS.slice(0, Math.max(...project.rows.map((r) => r.length - 1)));
    for (const tool of tools) head.append(toolHead(tool));
    const thead = el("thead");
    thead.append(head);
    table.append(thead);
    const body = el("tbody");
    for (const [scenario, ...values] of project.rows) {
      const tr = el("tr");
      const th = el("th", "scenario");
      th.append(el("code", undefined, scenario), el("span", "what", SCENARIOS[scenario] ?? ""));
      tr.append(th);
      const jrs = values[0];
      const best = Math.min(...values.filter((v): v is number => v != null));
      tools.forEach((_, i) => {
        const value = values[i];
        const td = el("td", i === 0 ? "jrs" : undefined);
        if (value === null || value === undefined) {
          td.textContent = "—";
        } else if (i === 0 || jrs === null) {
          td.textContent = format(value);
        } else {
          td.append(document.createTextNode(format(value)), el("span", "x", value < jrs ? faster(value, jrs) : ratio(value, jrs)));
          if (value < jrs) td.classList.add("ahead");
        }
        // The fastest tool in the row, jrs or not: in colour on the page,
        // and named for a screen reader, which cannot see it.
        if (value === best) {
          td.classList.add("best");
          td.append(el("span", "visually-hidden", " (fastest)"));
        }
        tr.append(td);
      });
      body.append(tr);
    }
    table.append(body);
    scroll.append(table);
    figure.append(scroll);
    host.append(figure);
  }
}

function svgEl<K extends keyof SVGElementTagNameMap>(tag: K, attrs: Record<string, string | number> = {}): SVGElementTagNameMap[K] {
  const node = document.createElementNS(SVG, tag);
  for (const [k, v] of Object.entries(attrs)) node.setAttribute(k, String(v));
  return node;
}

/** A round axis step that splits `max` into about three gridlines. */
function tickStep(max: number): number {
  const raw = max / 3;
  const pow = 10 ** Math.floor(Math.log10(raw));
  return [1, 2, 2.5, 5, 10].map((m) => m * pow).find((s) => s >= raw)!;
}

function change(now: number, before: number): string {
  const pct = Math.round((now / before - 1) * 100);
  return `${pct > 0 ? "+" : pct < 0 ? "−" : "±"}${Math.abs(pct)}% on ${format(before)}`;
}

/** The plot's own units: the SVG scales as a whole, text with it. */
const W = 340;
const H = 176;
const PAD = { top: 12, right: 44, bottom: 26, left: 48 };

/**
 * jrs's time in one scenario release by release, on a linear scale from zero,
 * with the warm Gradle daemon's as a reference line — left out, and said so,
 * where it would flatten jrs's against the floor.
 */
function trendChart(project: string, trend: Trend | undefined, scenario: string): HTMLElement {
  const figure = el("figure", "chart trend");
  const caption = el("figcaption");
  caption.append(el("strong", undefined, scenario), document.createTextNode(SCENARIOS[scenario] ?? ""));
  figure.append(caption);
  if (!trend) {
    figure.append(el("p", "trend-empty", "Not timed on a workspace."));
    return figure;
  }
  const [, jrs, gradle] = trend;
  const jrsMax = Math.max(...jrs);
  const showRef = Math.max(...gradle) <= jrsMax * 2.5;
  const step = tickStep(Math.max(jrsMax, ...(showRef ? gradle : [])));
  const top = Math.ceil(Math.max(jrsMax, ...(showRef ? gradle : [])) / step) * step;
  const x = (i: number) => PAD.left + (i * (W - PAD.left - PAD.right)) / (RELEASES.length - 1);
  const y = (v: number) => PAD.top + (1 - v / top) * (H - PAD.top - PAD.bottom);

  const svg = svgEl("svg", { viewBox: `0 0 ${W} ${H}`, role: "img" });
  svg.setAttribute(
    "aria-label",
    `jrs, ${scenario} on ${project}, by release: ` + RELEASES.map((r, i) => `${r} ${format(jrs[i]!)}`).join(", "),
  );
  for (let v = 0; v <= top; v += step) {
    svg.append(svgEl("line", { class: "grid", x1: PAD.left, x2: W - PAD.right + 8, y1: y(v), y2: y(v) }));
    const label = svgEl("text", { class: "axis", x: PAD.left - 8, y: y(v), "text-anchor": "end", "dominant-baseline": "middle" });
    label.textContent = v === 0 ? "0" : v < 1000 ? `${v} ms` : `${+(v / 1000).toFixed(2)} s`;
    svg.append(label);
  }
  RELEASES.forEach((r, i) => {
    const label = svgEl("text", { class: "axis release", x: x(i), y: H - 5, "text-anchor": "middle" });
    label.textContent = r;
    svg.append(label);
  });
  const path = (vs: number[]) => vs.map((v, i) => `${i ? "L" : "M"}${x(i).toFixed(1)},${y(v).toFixed(1)}`).join("");
  if (showRef) svg.append(svgEl("path", { class: "line ref", d: path(gradle) }));
  svg.append(svgEl("path", { class: "line jrs", d: path(jrs) }));
  const cross = svgEl("line", { class: "cross", y1: PAD.top, y2: H - PAD.bottom });
  svg.append(cross);
  const dots = jrs.map((v, i) => {
    const dot = svgEl("circle", { class: "dot", cx: x(i), cy: y(v), r: 4 });
    svg.append(dot);
    return dot;
  });
  // The latest release, labelled in place; the rest are a hover away.
  const last = jrs.length - 1;
  const end = svgEl("text", { class: "end", x: x(last) + 8, y: y(jrs[last]!), "dominant-baseline": "middle" });
  end.textContent = format(jrs[last]!);
  svg.append(end);

  const plot = el("div", "trend-plot");
  plot.tabIndex = 0;
  const tip = el("span", "bar-tip trend-tip");
  plot.append(svg, tip);
  figure.append(plot);
  if (!showRef) {
    figure.append(el("p", "trend-off", `Gradle daemon ${format(Math.min(...gradle))}–${format(Math.max(...gradle))}, off this scale`));
  }

  let shown = -1;
  const show = (i: number) => {
    shown = i;
    cross.setAttribute("x1", String(x(i)));
    cross.setAttribute("x2", String(x(i)));
    plot.classList.add("hover");
    dots.forEach((d, j) => d.classList.toggle("on", j === i));
    tip.replaceChildren();
    tip.append(el("strong", undefined, `jrs ${RELEASES[i]} · ${format(jrs[i]!)}`));
    if (i > 0) tip.append(el("span", undefined, change(jrs[i]!, jrs[i - 1]!)));
    tip.append(el("span", undefined, `Gradle daemon ${format(gradle[i]!)}`));
    const frac = x(i) / W;
    tip.style.left = `${frac * 100}%`;
    tip.style.transform = `translateX(${frac < 0.3 ? -15 : frac > 0.7 ? -85 : -50}%)`;
  };
  const hide = () => {
    shown = -1;
    plot.classList.remove("hover");
    dots.forEach((d) => d.classList.remove("on"));
  };
  plot.addEventListener("pointermove", (e) => {
    const box = svg.getBoundingClientRect();
    const at = ((e.clientX - box.left) / box.width) * W;
    let nearest = 0;
    RELEASES.forEach((_, i) => {
      if (Math.abs(x(i) - at) < Math.abs(x(nearest) - at)) nearest = i;
    });
    show(nearest);
  });
  plot.addEventListener("pointerleave", hide);
  plot.addEventListener("focus", () => show(last));
  plot.addEventListener("blur", hide);
  plot.addEventListener("keydown", (e) => {
    const move = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
    if (!move) return;
    e.preventDefault();
    show(Math.min(Math.max((shown < 0 ? last : shown) + move, 0), last));
  });
  return figure;
}

/** A tab per project above one chart per scenario, redrawn on a switch. */
function renderTrends(host: HTMLElement): void {
  const tabs = el("div", "tabs");
  tabs.setAttribute("role", "tablist");
  tabs.setAttribute("aria-label", "Project");
  const panel = el("div", "charts trends");
  panel.id = "trend-panel";
  panel.setAttribute("role", "tabpanel");
  // Only projects with a cell in every release; one new to the last pass has none.
  const projects = RESULTS.filter((p) => HISTORY[p.id]);
  const buttons = projects.map((p) => {
    const b = el("button", undefined, p.name);
    b.type = "button";
    b.id = `trend-${p.id}`;
    b.setAttribute("role", "tab");
    b.setAttribute("aria-controls", panel.id);
    b.title = p.what;
    return b;
  });
  const select = (i: number) => {
    const project = projects[i]!;
    buttons.forEach((b, j) => {
      b.setAttribute("aria-selected", String(i === j));
      b.tabIndex = i === j ? 0 : -1;
    });
    panel.setAttribute("aria-labelledby", buttons[i]!.id);
    const trends = HISTORY[project.id]!;
    panel.replaceChildren(
      ...Object.keys(SCENARIOS).map((s) => trendChart(project.name, trends.find((t) => t[0] === s), s)),
    );
  };
  buttons.forEach((b, i) => {
    b.addEventListener("click", () => select(i));
    b.addEventListener("keydown", (e) => {
      const step = e.key === "ArrowRight" ? 1 : e.key === "ArrowLeft" ? -1 : 0;
      if (!step) return;
      const next = (i + step + buttons.length) % buttons.length;
      select(next);
      buttons[next]!.focus();
    });
  });
  tabs.append(...buttons);
  host.append(tabs, panel);
  // gen-2000 by default: the project where most of the moves are largest.
  select(Math.max(projects.findIndex((p) => p.id === "gen-2000"), 0));
}

stampVersion();
setupCopy();
for (const host of $$<HTMLElement>("[data-bars]")) renderBars(host);
const trends = $<HTMLElement>("[data-trends]");
if (trends) renderTrends(trends);
const tables = $<HTMLElement>("[data-tables]");
if (tables) renderTables(tables);
