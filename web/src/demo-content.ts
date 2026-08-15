/**
 * Demo content for the "Add demo document" action (owner/author-gated button).
 *
 * This module is loaded via dynamic import() from main.ts's addDemoFile: it is
 * ~250 lines of seeded references plus a generated document body that have no
 * role in the critical startup path, so they stay out of the main bundle and
 * are only fetched when the button is actually clicked.
 */

/** Seeded bibliography for the demo study; each becomes a real reference row. */
export const DEMO_REFERENCE_TITLES: readonly string[] = [
  "Honeywell, B. (2023). Observational Evidence of Mustelid-Fey Synchronization at Subterranean Frequency Events.",
  "Glimmerwick, T. (2022). Spectral Analysis of Bioluminescent Dance Floors in the Fairy Underground.",
  "Badgerton, M. (2024). Aggression and Rhythm: Behavioral Correlates in Mellivora capensis at Rave Sites.",
  "Sparkletoes, P. (2023). Echolocation Interference by Fairy Folk During High-BPM Audio Playback.",
  "Hufflepaw, D. (2024). Dietary Shifts in Honey Badgers Attending Nocturnal Fairy Gatherings: A Pilot Study.",
  "Moonwhisper, L. (2022). The Underground Sound: Acoustic Architecture of Fairy Rave Caverns.",
  "Clawson, R. (2023). Territorial Marking Behavior Overlaid with Glitter Residue: A Forensic Approach.",
  "Twinkleburst, F. (2024). Effects of Sustained 140 BPM Exposure on Mustelid Heart Rate and Fairy Wing Beat Frequency."
]

/** Generates a substantial Typst document about honey badgers and fairy raves. */
export function generateDemoBody(refIds: string[]): string {
  const L: string[] = []
  L.push('#set page(paper: "a4", margin: (x: 2cm, y: 2.5cm))')
  L.push('#set text(size: 10pt)')
  L.push('#set par(justify: true)')
  L.push("")
  L.push("= Honey Badgers and the Fairy Underground Rave Scene: A Comprehensive Investigation")
  L.push("")
  L.push("_By the Institute for Interdisciplinary Crypto-Zoological Acoustics_")
  L.push("")
  L.push("== Abstract")
  L.push("")
  L.push("This study presents the first systematic investigation into the observed relationship between the honey badger (*Mellivora capensis*) and the previously undocumented fairy underground rave scene. Over a period of 18 months, our research team deployed motion-activated cameras, acoustic sensors, and enchanted monitoring equipment across 47 suspected fairy rave sites in the Welsh countryside. Our findings reveal a startling pattern of honey badger attendance at these events, characterized by sustained rhythmic head-bobbing, aggressive dance floor territoriality, and an unexplained tolerance for glitter. See @fig-attendance for the spatial distribution of observed encounters.")
  L.push("")
  if (refIds[0]) L.push(`The phenomenon was first reported by ${"Honeywell"} #cite(<${refIds[0]}>) and initially dismissed as a statistical artifact.`)
  if (refIds[1]) L.push(`However, subsequent spectral analysis of the bioluminescent dance floors #cite(<${refIds[1]}>) confirmed that the acoustic signatures were consistent across all sites.`)
  L.push("")

  L.push("== Introduction")
  L.push("")
  L.push("=== Background")
  L.push("The honey badger, long renowned for its fearlessness and general indifference to consequences, has not previously been associated with subterranean recreational activities. Fairy folk, conversely, are well-documented in their preference for underground gatherings featuring synchronized bioluminescent light displays and rhythmic audio at tempos exceeding 130 BPM. The intersection of these two populations was first noted during a routine badger-tracking expedition in 2022, when field researcher Dr. B. Honeywell observed what she described as 'a large mustelid displaying unmistakable rhythmic coordination with a pulsating wall of fairy lights approximately 40 meters below a Welsh hillside.'")
  L.push("")
  L.push("=== Research Questions")
  L.push("This study addresses three primary questions:")
  L.push("+ Are honey badgers genuinely attending fairy raves, or is the observed proximity coincidental?")
  L.push("+ If attending, what behavioral modifications do honey badgers exhibit in the rave environment?")
  L.push("+ What is the ecological significance of this interspecies interaction?")
  L.push("")
  if (refIds[2]) L.push(`Preliminary behavioral analysis #cite(<${refIds[2]}>) suggests the attendance is deliberate and sustained.`)
  L.push("")

  L.push("== Methods")
  L.push("")
  L.push("=== Study Sites")
  L.push("We identified 47 candidate fairy rave sites based on surface indicators (unusual concentrations of toadstools in geometric patterns, faint bass vibrations detectable at ground level, and intermittent glitter deposits on nearby vegetation). Of these, 31 sites showed confirmed activity during the study period. The geographic distribution of confirmed sites is shown in @fig-attendance.")
  L.push("")
  L.push("=== Monitoring Equipment")
  L.push("Each site was instrumented with:")
  L.push("+ Motion-activated infrared cameras (Reconyx HyperFire 2) modified for subterranean deployment")
  L.push("+ Acoustic sensors capable of capturing frequencies from 10 Hz to 80 kHz, covering both the fairy audio range and the full honey badger vocalization spectrum")
  L.push("+ Enchanted monitoring crystals (for bioluminescent intensity and fairy-aura detection), provided by our Department of Fey Engineering")
  L.push("+ Glitter-spectroscopy collection pads placed at 5-meter intervals along suspected badger transit tunnels")
  L.push("")

  // Table 1: Study sites
  L.push("=== Site Characteristics")
  L.push("")
  L.push("#figure(table(")
  L.push("  columns: 4,")
  L.push("  [*Site*], [*Depth (m)*], [*Avg BPM*], [*Badger Visits*],")
  const sites = [
    ["Cwm Derwen", "38", "142", "17"],
    ["Tywyn Hollow", "52", "138", "23"],
    ["Blaenau Cavern", "41", "145", "9"],
    ["Ystrad Tunnel", "67", "150", "31"],
    ["Pen-y-Fawr Sink", "29", "135", "12"],
    ["Coed Ystlum", "44", "148", "8"],
    ["Nant Gwrhyd", "55", "141", "19"],
    ["Ogof Tinker", "33", "139", "25"],
    ["Ffos-y-Ffridd", "48", "146", "14"],
    ["Bwlch Glas", "61", "152", "7"],
  ]
  for (const [name, depth, bpm, visits] of sites) {
    L.push(`  [${name}], [${depth}], [${bpm}], [${visits}],`)
  }
  L.push(`), caption: [Site characteristics across all 10 primary monitoring locations. Average BPM measured at peak activity (midnight to 3 AM). Badger visits counted over the 18-month study period.])`)
  L.push("<tbl-sites>")
  L.push("The complete site data is presented in @tbl-sites. Note the positive correlation between site depth and average BPM (Pearson r = 0.72, p < 0.01), suggesting that deeper fairy venues favor faster tempos.")
  L.push("")

  // Figure 1
  L.push("=== Spatial Distribution")
  L.push("")
  L.push("#figure(rect(width: 100%, height: 8cm, fill: luma(240), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Map of confirmed fairy rave sites with honey badger attendance overlay. Each dot represents a confirmed site; dot size proportional to badger visit frequency.])), caption: [Geographic distribution of confirmed fairy rave sites (n=31) and honey badger encounter frequency. Sites concentrated in upland Wales, with a secondary cluster in the Brecon Beacons.])")
  L.push("<fig-attendance>")
  L.push("")

  L.push("== Results")
  L.push("")
  L.push("=== Honey Badger Attendance Patterns")
  L.push("")
  const behaviors = [
    ["Rhythmic head-bobbing", "94%", "Sustained bobbing at the dominant BPM for periods exceeding 15 minutes"],
    ["Territorial dance-floor marking", "78%", "Scent-marking posts adjacent to the primary bioluminescent wall"],
    ["Glitter tolerance", "100%", "No adverse reactions observed despite heavy glitter accumulation on fur"],
    ["Interspecies proximity tolerance", "88%", "Honey badgers remained within 2m of fairy folk without aggression"],
    ["Bioluminescent interaction", "67%", "Direct contact with fairy light displays (nose-touching, pawing)"],
    ["Sustained stillness during breakdowns", "91%", "Complete immobility during musical 'drops' followed by explosive activity"],
    ["Vocalization synchronization", "45%", "Growling patterns that coincided with bass drops on 45% of observed occasions"],
    ["Post-event napping", "82%", "Badgers remained at the site for an average of 47 minutes after music ceased"],
  ]
  L.push("#figure(table(")
  L.push("  columns: 3,")
  L.push("  [*Behavior*], [*Frequency*], [*Description*],")
  for (const [beh, freq, desc] of behaviors) {
    L.push(`  [${beh}], [${freq}], [${desc}],`)
  }
  L.push(`), caption: [Observed honey badger behaviors at fairy rave sites (n=165 encounters across 31 sites). Frequency represents the percentage of encounters in which the behavior was observed at least once.])`)
  L.push("<tbl-behaviors>")
  L.push("")
  L.push("The behavioral data summarized in @tbl-behaviors reveals that honey badgers exhibit a remarkably consistent suite of rave-related behaviors. The 100% glitter tolerance rate is particularly noteworthy, as honey badgers are typically averse to foreign substances on their fur.")
  L.push("")
  if (refIds[4]) L.push(`Hufflepaw's dietary analysis #cite(<${refIds[4]}>) further revealed that attending badgers showed a 34% increase in caloric intake in the 24 hours following a rave event, suggesting substantial energy expenditure.`)
  L.push("")

  // Figure 2
  L.push("=== Bioluminescent Interaction Analysis")
  L.push("")
  L.push("#figure(rect(width: 100%, height: 7cm, fill: luma(245), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Bioluminescent intensity (lux) over a typical 3-hour fairy rave event, with honey badger proximity events marked as vertical lines. Note the clustering of badger approaches during peak luminescence.])), caption: [Temporal relationship between bioluminescent intensity and honey badger proximity events during a representative rave event at Ystrad Tunnel (Site 4). Peak intensity events consistently attracted badger approach within 30 seconds.])")
  L.push("<fig-bioluminescence>")
  L.push("")
  L.push("As shown in @fig-bioluminescence, honey badgers demonstrated a clear attraction to peak bioluminescent events. The mean approach latency was 22.4 seconds (SD = 8.1), suggesting a rapid response to visual stimuli rather than acoustic cues alone.")
  L.push("")

  // Table 3
  L.push("=== Acoustic Analysis")
  L.push("")
  L.push("Acoustic recordings revealed an unexpected finding: honey badgers at rave sites produced vocalizations in the 40-60 Hz range that were phase-locked to the dominant bass frequency of the fairy audio system. This synchronization was observed at 74% of encounters and is unprecedented in the mustelid acoustic literature.")
  L.push("")
  if (refIds[7]) L.push(`The sustained 140+ BPM exposure documented by Twinkleburst #cite(<${refIds[7]}>) may explain the elevated heart rates observed in attending badgers (mean: 142 BPM vs. baseline 78 BPM).`)
  L.push("")
  const acoustic = [
    ["Site 1 (Cwm Derwen)", "142", "48 Hz", "Yes", "0.91"],
    ["Site 2 (Tywyn Hollow)", "138", "45 Hz", "Yes", "0.88"],
    ["Site 4 (Ystrad Tunnel)", "150", "52 Hz", "Yes", "0.95"],
    ["Site 8 (Ogof Tinker)", "139", "44 Hz", "No", "—"],
    ["Site 10 (Bwlch Glas)", "152", "55 Hz", "Yes", "0.97"],
  ]
  L.push("#figure(table(")
  L.push("  columns: 5,")
  L.push("  [*Site*], [*BPM*], [*Badger vocal freq*], [*Phase-locked*], [*Coherence*],")
  for (const [site, bpm, freq, locked, coh] of acoustic) {
    L.push(`  [${site}], [${bpm}], [${freq}], [${locked}], [${coh}],`)
  }
  L.push(`), caption: [Acoustic analysis of honey badger vocalizations at fairy rave sites. Phase-locking assessed via cross-correlation of badger vocalization envelopes with the fairy audio bass frequency. Coherence values >0.8 indicate strong synchronization.])`)
  L.push("<tbl-acoustic>")
  L.push("")

  L.push("== Discussion")
  L.push("")
  L.push("=== Why Do Honey Badgers Attend Fairy Raves?")
  L.push("")
  L.push("Several hypotheses may explain this unprecedented interspecies interaction:")
  L.push("")
  L.push("1. _Acoustic attraction_: The low-frequency bass characteristic of fairy rave music falls within the honey badger's peak hearing sensitivity. The sustained rhythm may produce a entrainment effect analogous to the 'groove response' documented in humans.")
  L.push("")
  L.push("2. _Thermoregulatory benefit_: Underground sites maintain a stable 12-15 degrees Celsius year-round, providing thermal refuge. The combination of stable temperature and rhythmic stimulation may create an optimal resting environment.")
  L.push("")
  if (refIds[3]) L.push(`3. _Fairy aura interaction_: Sparkletoes' work on echolocation interference #cite(<${refIds[3]}>) suggests fairy folk emit a subtle electromagnetic field. Honey badgers, with their large sinus cavities, may be uniquely positioned to detect and find this field pleasant.`)
  L.push("")
  L.push("4. _Glitter as a tracking mechanism_: The observation that honey badgers accumulate significant glitter without distress raises the possibility that glitter serves as a visual marker system. Honey badgers may use glitter trails to navigate between rave sites, effectively creating a glitter-based geographic information system.")
  L.push("")

  L.push("=== Ecological Implications")
  L.push("")
  L.push("The presence of an apex mustelid at fairy social events has potential implications for both populations:")
  L.push("")
  L.push("+ For fairy folk: the honey badger's territorial behavior may influence dance floor layout and crowd dynamics. Observations of fairies voluntarily yielding space to approaching badgers suggest a established interspecies social hierarchy.")
  L.push("+ For honey badgers: sustained exposure to high-BPM environments and bioluminescent stimuli may have long-term physiological effects. The elevated heart rates documented during events warrant further investigation.")
  L.push("+ For the ecosystem: the glitter deposition patterns associated with badger transit between sites may affect soil composition and plant growth along transit corridors.")
  L.push("")
  if (refIds[5]) L.push(`The acoustic architecture of the fairy caverns #cite(<${refIds[5]}>) creates natural amplification chambers that may extend the effective range of the rave signal, attracting badgers from distances exceeding 5 km.`)
  if (refIds[6]) L.push(`Clawson's forensic analysis of territorial markings #cite(<${refIds[6]}>) confirmed that 89% of marked posts within rave sites contained both badger scent compounds and fairy glitter particles, providing physical evidence of sustained co-occupation.`)
  L.push("")

  // Figure 3
  L.push("#figure(rect(width: 100%, height: 6cm, fill: luma(242), stroke: 0.5pt, align(center + horizon, text(10pt, gray)[Hypothesized model of honey badger-fairy rave interaction. Arrows indicate proposed causal relationships. Dashed lines represent uncertain pathways requiring further investigation.])), caption: [Conceptual model integrating acoustic attraction, thermoregulatory benefit, fairy-aura detection, and glitter-based navigation into a unified framework for understanding the honey badger-fairy rave phenomenon.])")
  L.push("<fig-model>")
  L.push("The integrated model proposed in @fig-model suggests that the interaction is maintained by positive feedback loops rather than a single attractor.")
  L.push("")

  L.push("== Limitations")
  L.push("")
  L.push("This study has several limitations that should be addressed in future research:")
  L.push("")
  L.push("- The enchanted monitoring crystals have not been independently calibrated against non-enchanted references.")
  L.push("- Glitter-spectroscopy is an emerging methodology with no established protocols for mustelid-associated glitter analysis.")
  L.push("- The geographic scope was limited to Wales; fairy rave sites in other regions (Cornwall, the Scottish Highlands, the Isle of Man) may exhibit different patterns.")
  L.push("- Observer bias may exist, as all field researchers reported finding the observations 'absolutely delightful' and may have unconsciously sought confirming evidence.")
  L.push("- The sample size of 165 encounters, while substantial, is insufficient for robust population-level inference.")
  L.push("")

  L.push("== Conclusions")
  L.push("")
  L.push("This study provides the first systematic evidence that honey badgers deliberately attend and actively participate in the fairy underground rave scene. The observed behaviors — rhythmic synchronization, bioluminescent interaction, glitter tolerance, and territorial dance-floor marking — constitute a coherent behavioral syndrome that warrants recognition as a distinct ecological phenomenon. We propose the term _Mellivora rava_ (rave badger syndrome) to describe this behavioral pattern.")
  L.push("")
  L.push("Future research should focus on: (1) physiological monitoring of attending badgers via non-invasive biotelemetry, (2) experimental manipulation of BPM and bioluminescent intensity to establish causal relationships, and (3) genetic analysis to determine whether rave attendance has a heritable component.")
  L.push("")

  L.push("== Acknowledgments")
  L.push("")
  L.push("We thank the Welsh Fairy Council for permitting access to monitoring sites, the Badger Watch volunteer network for field assistance, and the Department of Fey Engineering for the enchanted monitoring crystals. This research was supported by a grant from the Institute for Interdisciplinary Crypto-Zoological Acoustics (Grant No. HBFR-2023-007). No honey badgers or fairy folk were harmed during this study, though three cameras were destroyed by enthusiastic badger interactions.")
  L.push("")

  return L.join("\n")
}
