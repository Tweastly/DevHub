# The installer under its former name (ghcr.io/tweastly/devhub:install), for the commands of
# older docs: DevHub's own image, started on `devhub install`. The usual command needs no
# special tag, DevHub's image installs itself when run bare with the Docker socket:
#
#   docker run --rm -it -v /var/run/docker.sock:/var/run/docker.sock ghcr.io/tweastly/devhub
ARG BASE=ghcr.io/tweastly/devhub:latest
FROM ${BASE}
ARG BASE
# What it installs: the image it was made from (not this one, whose command is the installer).
ENV DEVHUB_IMAGE=${BASE}
ENTRYPOINT ["devhub", "install"]
