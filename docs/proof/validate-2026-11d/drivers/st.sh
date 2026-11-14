cd /srv/euroos/final-runs
echo "--- $(date +%T)"
for f in runs/*.out wizard.out rp.out q35/matrix.out; do echo "## $f: $(tail -n 1 $f | cut -c1-170)"; done
cat runs/*.done wizard.done rp.done boots.done bt.done 2>/dev/null | tr '\n' ' '; echo
tail -n 3 tcg/f/queue.txt; echo "short: a=$(ls short-a/boot-*.log 2>/dev/null | wc -l) b=$(ls short-b/boot-*.log 2>/dev/null | wc -l)"
tail -n 2 soak/out.txt | cut -c1-200
ls bt/*.tlog 2>/dev/null | wc -l
