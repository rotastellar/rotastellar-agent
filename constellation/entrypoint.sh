#!/bin/bash
# Launch RS-LEO constellation — one agent process per satellite.
# Reads satellite definitions from /etc/rotastellar/satellites.json
# and starts each as a background process with staggered startup.

set -e

CONFIG="/etc/rotastellar/satellites.json"
SATELLITE_COUNT=$(jq length "$CONFIG")

echo "=== RotaStellar Constellation ==="
echo "Satellites: $SATELLITE_COUNT"
echo "API:        $API_URL"
echo "Sim:        $SIM_URL"
echo "================================="

PIDS=()

cleanup() {
    echo "Shutting down constellation..."
    for pid in "${PIDS[@]}"; do
        kill "$pid" 2>/dev/null || true
    done
    wait
    echo "All agents stopped."
}

trap cleanup SIGTERM SIGINT

for i in $(seq 0 $((SATELLITE_COUNT - 1))); do
    AGENT_ID=$(jq -r ".[$i].agent_id" "$CONFIG")
    SAT_ID=$(jq -r ".[$i].satellite_id" "$CONFIG")
    SAT_NAME=$(jq -r ".[$i].satellite_name" "$CONFIG")
    ELEMENTS=$(jq -c ".[$i].orbital_elements" "$CONFIG")

    echo "Starting agent: $SAT_NAME ($AGENT_ID)"

    rotastellar-agent run \
        --agent-id "$AGENT_ID" \
        --api-url "$API_URL" \
        --api-key "$API_KEY" \
        --sim-url "$SIM_URL" \
        --satellite-id "$SAT_ID" \
        --satellite-name "$SAT_NAME" \
        --orbital-elements "$ELEMENTS" \
        --poll-interval 30 &

    PIDS+=($!)

    # Stagger startup by 2 seconds
    if [ $i -lt $((SATELLITE_COUNT - 1)) ]; then
        sleep 2
    fi
done

echo "All $SATELLITE_COUNT agents launched."

# Wait for any process to exit
wait -n
EXIT_CODE=$?

echo "Agent exited with code $EXIT_CODE, shutting down..."
cleanup
exit $EXIT_CODE
