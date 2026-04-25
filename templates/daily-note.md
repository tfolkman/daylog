---
date: DATE_PLACEHOLDER
sleep:                    # e.g., 10:30pm-6:15am
sleep_quality:            # 1-5
mood:                     # 1-5
energy:                   # 1-5
weight:                   # WEIGHT_UNIT_PLACEHOLDER

type:                     # session type: lifting, cardio, climbing, mixed, rest
week:                     # training week number
block:                    # training block: volume, intensity, peak, deload
duration:                 # minutes
rpe:                      # 1-10

lifts:
  # exercise: weight x reps, weight x reps
  # Examples:
  #   squat: 185x5, 205x3, 225x1
  #   pullup: BWx8, BWx6
  #   rdl: 135x8x3            (weight x reps x sets)

# Custom metrics (any [metrics] key from config)
# resting_hr: 52
# meditation_min: 15

# Climbing (requires [modules] climbing = true)
# climbs:
#   board: gym                # gym/moon/kilter/tension
#   sends:
#     - V5
#     - V4 x2
#   attempts:
#     - V7
---

## Notes

