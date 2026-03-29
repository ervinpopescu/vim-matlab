local ok, matlab = pcall(require, "vim-matlab")
if ok then
  matlab.attach()
end
