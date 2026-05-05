#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 2 ]]; then
  echo "Usage: scripts/snapshot_to_svg.sh <input.txt> <output.svg> [title]" >&2
  exit 2
fi

IN_PATH="$1"
OUT_PATH="$2"
TITLE="${3:-}"

mkdir -p "$(dirname "${OUT_PATH}")"

perl -CS -Mutf8 -e '
use strict;
use warnings;
use utf8;
use open qw(:std :encoding(UTF-8));

my ($in_path, $title) = @ARGV;
open my $fh, "<", $in_path or die "open $in_path: $!";
my @lines = <$fh>;
chomp @lines;

sub esc_xml {
  my ($s) = @_;
  $s =~ s/&/&amp;/g;
  $s =~ s/</&lt;/g;
  $s =~ s/>/&gt;/g;
  return $s;
}

my $max = 0;
for my $line (@lines) {
  my $len = length($line);
  $max = $len if $len > $max;
}

my $font_px = 14;
my $line_h = 18;
my $pad = 16;
my $char_w = 0.62; # heuristic for monospace in SVG
my $width = int($pad * 2 + ($max * $font_px * $char_w) + 0.5);
my $height = int($pad * 2 + (@lines * $line_h) + $font_px + 0.5);

my $bg = "#0b1220";   # deep slate
my $fg = "#e2e8f0";   # slate-200
my $dim = "#94a3b8";  # slate-400

print qq{<?xml version="1.0" encoding="UTF-8"?>\n};
print qq{<svg xmlns="http://www.w3.org/2000/svg" width="$width" height="$height" viewBox="0 0 $width $height">\n};
if (defined($title) && length($title)) {
  my $t = esc_xml($title);
  print qq{  <title>$t</title>\n};
}
print qq{  <rect x="0" y="0" width="$width" height="$height" rx="12" fill="$bg"/>\n};
print qq{  <text x="$pad" y="} . ($pad + $font_px) . qq{" font-size="$font_px" fill="$fg" font-family="ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, \\"Liberation Mono\\", \\"Courier New\\", monospace" xml:space="preserve">\n};

my $first = 1;
for my $line (@lines) {
  my $escaped = esc_xml($line);
  my $dy = $first ? 0 : $line_h;
  $first = 0;
  print qq{    <tspan x="$pad" dy="$dy">$escaped</tspan>\n};
}

print qq{  </text>\n};
print qq{  <text x="$pad" y="} . ($height - 10) . qq{" font-size="11" fill="$dim" font-family="ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, \\"Liberation Mono\\", \\"Courier New\\", monospace">generated from $in_path</text>\n};
print qq{</svg>\n};
' "${IN_PATH}" "${TITLE}" > "${OUT_PATH}"

echo "wrote ${OUT_PATH}"

